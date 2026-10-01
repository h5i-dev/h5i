//! Account and record rules of the Atuin sync server
//! (github.com/atuinsh/atuin, crates/atuin-server at 5b10eb0) as an h5i-app kernel.
//!
//! Password and token hashing stay in the shell: commands carry hashes and
//! "the password matched" flags. Host ids and record tags are numbers here.
//! `transition` requires the password to delete an account (issue #3297);
//! `transition_current` matches Atuin today, which does not.

pub type Text = Vec<u8>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Principal {
    Anonymous,
    User(u64),
}

h5i_app_schema::schema! {
    mapping atuin_tables for atuin_kernel, writes Write, lean "../proofs/generated/Schema.lean";

    #[derive(Clone, Debug, PartialEq, Eq, Default)]
    pub struct Snapshot {
        counter: Counter,
        settings: Settings,
        users: Vec<User>,
        sessions: Vec<Session>,
        records: Vec<Record>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct User in "users" {
        key { id: u64 }
        username: Text,
        password: Text,
    }

    /// One session per user, created at registration (`add_user_with_session`).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Session in "sessions" {
        key { user: u64 }
        token: Text,
    }

    /// An encrypted record; the server never reads `data`, only its size.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Record in "records" {
        key { user: u64, host: u64, tag: u64, idx: u64 }
        data: Text,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Default)]
    pub struct Settings in "settings" {
        key {}
        open_registration: bool,
        /// 0 means no limit, as in Atuin.
        max_record_size: u64,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Default)]
    pub struct Counter in "counters" {
        key {}
        next_id: u64,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewRecord {
    pub host: u64,
    pub tag: u64,
    pub idx: u64,
    pub data: Text,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// `password` and `token` are hashes made by the shell.
    Register {
        username: Text,
        password: Text,
        token: Text,
    },
    /// `current_ok`: the shell checked the current password.
    ChangePassword {
        current_ok: bool,
        new_password: Text,
    },
    /// `password_ok`: the shell checked the password, if the client sent one.
    DeleteAccount {
        password_ok: bool,
    },
    AddRecords {
        records: Vec<NewRecord>,
    },
    NextRecords {
        host: u64,
        tag: u64,
        start: u64,
        count: u64,
    },
    Status,
    DeleteStore,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    PutUser(User),
    DelUser(u64),
    PutSession(Session),
    DelSession(u64),
    PutRecord(Record),
    /// Delete every record of a user.
    DelRecordsOf(u64),
    SetCounter(Counter),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Registered(u64),
    Done,
    Records(Vec<Record>),
    /// (host, tag, idx) of each of the caller's records.
    Status(Vec<(u64, u64, u64)>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Unauthenticated,
    RegistrationClosed,
    BadUsername,
    UsernameTaken,
    RecordTooLarge,
    WrongPassword,
    Overflow,
}

/// Alphanumeric or hyphen, as in Atuin's `register`. A separate function so
/// the extracted Lean gets a `Bool` result type.
fn byte_ok(c: u8) -> bool {
    if c >= b'a' && c <= b'z' {
        return true;
    }
    if c >= b'A' && c <= b'Z' {
        return true;
    }
    if c >= b'0' && c <= b'9' {
        return true;
    }
    c == b'-'
}

fn username_ok(name: &Text) -> bool {
    let mut i = 0;
    while i < name.len() {
        if !byte_ok(name[i]) {
            return false;
        }
        i += 1;
    }
    true
}

fn username_taken(users: &Vec<User>, name: &Text) -> bool {
    let mut i = 0;
    while i < users.len() {
        if users[i].username == *name {
            return true;
        }
        i += 1;
    }
    false
}

pub fn user_exists(users: &Vec<User>, id: u64) -> bool {
    let mut i = 0;
    while i < users.len() {
        if users[i].id == id {
            return true;
        }
        i += 1;
    }
    false
}

fn find_user(users: &Vec<User>, id: u64) -> Option<User> {
    let mut i = 0;
    while i < users.len() {
        if users[i].id == id {
            return Some(users[i].clone());
        }
        i += 1;
    }
    None
}

/// Size cap check for one record; 0 means no limit.
fn fits(max: u64, r: &NewRecord) -> bool {
    max == 0 || (r.data.len() as u64) <= max
}

fn all_fit(max: u64, rs: &Vec<NewRecord>) -> bool {
    let mut i = 0;
    while i < rs.len() {
        if !fits(max, &rs[i]) {
            return false;
        }
        i += 1;
    }
    true
}

/// The caller's records in one series from index `start`, at most `count`.
fn next_records(
    rs: &Vec<Record>,
    user: u64,
    host: u64,
    tag: u64,
    start: u64,
    count: u64,
) -> Vec<Record> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < rs.len() {
        let r = &rs[i];
        if r.user == user
            && r.host == host
            && r.tag == tag
            && r.idx >= start
            && (out.len() as u64) < count
        {
            out.push(r.clone());
        }
        i += 1;
    }
    out
}

fn status_of(rs: &Vec<Record>, user: u64) -> Vec<(u64, u64, u64)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < rs.len() {
        if rs[i].user == user {
            out.push((rs[i].host, rs[i].tag, rs[i].idx));
        }
        i += 1;
    }
    out
}

fn to_writes(user: u64, rs: &Vec<NewRecord>) -> Vec<Write> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < rs.len() {
        let r = &rs[i];
        out.push(Write::PutRecord(Record {
            user,
            host: r.host,
            tag: r.tag,
            idx: r.idx,
            data: r.data.clone(),
        }));
        i += 1;
    }
    out
}

/// The signed-in user, if their account still exists.
fn signed_in(snap: &Snapshot, actor: &Principal) -> Option<u64> {
    match actor {
        Principal::User(id) => {
            if user_exists(&snap.users, *id) {
                Some(*id)
            } else {
                None
            }
        }
        Principal::Anonymous => None,
    }
}

fn one(w: Write) -> Vec<Write> {
    let mut v = Vec::new();
    v.push(w);
    v
}

fn step(
    actor: &Principal,
    snap: &Snapshot,
    cmd: &Command,
    require_reauth: bool,
) -> Result<(Vec<Write>, Reply), Error> {
    match cmd {
        Command::Register {
            username,
            password,
            token,
        } => {
            if !snap.settings.open_registration {
                return Err(Error::RegistrationClosed);
            }
            if !username_ok(username) {
                return Err(Error::BadUsername);
            }
            if username_taken(&snap.users, username) {
                return Err(Error::UsernameTaken);
            }
            let id = snap.counter.next_id;
            if id == u64::MAX {
                return Err(Error::Overflow);
            }
            let mut ws = Vec::new();
            ws.push(Write::SetCounter(Counter { next_id: id + 1 }));
            ws.push(Write::PutUser(User {
                id,
                username: username.clone(),
                password: password.clone(),
            }));
            ws.push(Write::PutSession(Session {
                user: id,
                token: token.clone(),
            }));
            Ok((ws, Reply::Registered(id)))
        }
        Command::ChangePassword {
            current_ok,
            new_password,
        } => {
            let uid = match signed_in(snap, actor) {
                Some(u) => u,
                None => return Err(Error::Unauthenticated),
            };
            if !*current_ok {
                return Err(Error::WrongPassword);
            }
            let u = match find_user(&snap.users, uid) {
                Some(u) => u,
                None => return Err(Error::Unauthenticated),
            };
            let changed = User {
                id: u.id,
                username: u.username,
                password: new_password.clone(),
            };
            Ok((one(Write::PutUser(changed)), Reply::Done))
        }
        Command::DeleteAccount { password_ok } => {
            let uid = match signed_in(snap, actor) {
                Some(u) => u,
                None => return Err(Error::Unauthenticated),
            };
            if require_reauth && !*password_ok {
                return Err(Error::WrongPassword);
            }
            let mut ws = Vec::new();
            ws.push(Write::DelSession(uid));
            ws.push(Write::DelRecordsOf(uid));
            ws.push(Write::DelUser(uid));
            Ok((ws, Reply::Done))
        }
        Command::AddRecords { records } => {
            let uid = match signed_in(snap, actor) {
                Some(u) => u,
                None => return Err(Error::Unauthenticated),
            };
            if !all_fit(snap.settings.max_record_size, records) {
                return Err(Error::RecordTooLarge);
            }
            Ok((to_writes(uid, records), Reply::Done))
        }
        Command::NextRecords {
            host,
            tag,
            start,
            count,
        } => {
            let uid = match signed_in(snap, actor) {
                Some(u) => u,
                None => return Err(Error::Unauthenticated),
            };
            Ok((
                Vec::new(),
                Reply::Records(next_records(
                    &snap.records,
                    uid,
                    *host,
                    *tag,
                    *start,
                    *count,
                )),
            ))
        }
        Command::Status => {
            let uid = match signed_in(snap, actor) {
                Some(u) => u,
                None => return Err(Error::Unauthenticated),
            };
            Ok((Vec::new(), Reply::Status(status_of(&snap.records, uid))))
        }
        Command::DeleteStore => {
            let uid = match signed_in(snap, actor) {
                Some(u) => u,
                None => return Err(Error::Unauthenticated),
            };
            Ok((one(Write::DelRecordsOf(uid)), Reply::Done))
        }
    }
}

/// With the fix requested in issue #3297: deleting an account needs the password.
pub fn transition(
    actor: &Principal,
    snap: &Snapshot,
    cmd: &Command,
) -> Result<(Vec<Write>, Reply), Error> {
    step(actor, snap, cmd, true)
}

/// Atuin today: a valid session alone can delete the account.
pub fn transition_current(
    actor: &Principal,
    snap: &Snapshot,
    cmd: &Command,
) -> Result<(Vec<Write>, Reply), Error> {
    step(actor, snap, cmd, false)
}

/// The column a user's records are deleted by: `user`, first in the key.
const RECORD_USER: u32 = 0;

/// One write's effect; `schema!`'s `apply` runs it over a write set.
pub fn apply_write(s: &mut Snapshot, w: Write) {
    match w {
        Write::PutUser(u) => User::put(&mut s.users, u),
        Write::DelUser(id) => s.users = User::del(&s.users, id),
        Write::PutSession(x) => Session::put(&mut s.sessions, x),
        Write::DelSession(u) => s.sessions = Session::del(&s.sessions, u),
        Write::PutRecord(r) => Record::put(&mut s.records, r),
        Write::DelRecordsOf(u) => s.records = Record::del_where(&s.records, RECORD_USER, &h5i_app_sql::Column::to_val(&u)),
        Write::SetCounter(c) => s.counter = c,
    }
}

/// The table writes one write makes.
fn sql_write(w: &Write, out: &mut Vec<h5i_app_sql::Write>) {
    match w {
        Write::PutUser(u) => out.push(u.sql_put()),
        Write::DelUser(id) => out.push(User::sql_del(*id)),
        Write::PutSession(x) => out.push(x.sql_put()),
        Write::DelSession(u) => out.push(Session::sql_del(*u)),
        Write::PutRecord(r) => out.push(r.sql_put()),
        Write::DelRecordsOf(u) => out.push(Record::sql_del_where(RECORD_USER, h5i_app_sql::Column::to_val(u))),
        Write::SetCounter(c) => out.push(c.sql_put()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> Snapshot {
        Snapshot {
            settings: Settings {
                open_registration: true,
                max_record_size: 4,
            },
            ..Default::default()
        }
    }

    fn run(s: &Snapshot, a: Principal, c: Command) -> (Snapshot, Result<Reply, Error>) {
        match transition(&a, s, &c) {
            Ok((ws, r)) => (apply(s, &ws), Ok(r)),
            Err(e) => (s.clone(), Err(e)),
        }
    }

    #[test]
    fn delete_needs_password_and_removes_everything() {
        let reg = Command::Register {
            username: b"ann".to_vec(),
            password: b"h".to_vec(),
            token: b"t".to_vec(),
        };
        let (s, r) = run(&open(), Principal::Anonymous, reg);
        assert_eq!(r, Ok(Reply::Registered(0)));
        let rec = NewRecord {
            host: 1,
            tag: 1,
            idx: 0,
            data: b"abc".to_vec(),
        };
        let (s, _) = run(
            &s,
            Principal::User(0),
            Command::AddRecords { records: vec![rec] },
        );
        assert_eq!(s.records.len(), 1);
        let (_, r) = run(
            &s,
            Principal::User(0),
            Command::DeleteAccount { password_ok: false },
        );
        assert_eq!(r, Err(Error::WrongPassword));
        assert!(transition_current(
            &Principal::User(0),
            &s,
            &Command::DeleteAccount { password_ok: false }
        )
        .is_ok());
        let (s, r) = run(
            &s,
            Principal::User(0),
            Command::DeleteAccount { password_ok: true },
        );
        assert_eq!(r, Ok(Reply::Done));
        assert!(s.users.is_empty() && s.sessions.is_empty() && s.records.is_empty());
    }
}
