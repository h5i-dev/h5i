//! Notes in an organization, with members and admins, whose policy is
//! declared in `schema!` rather than written as Rust.
//!
//! A member writes notes; a note is visible to everyone if it is public, and
//! otherwise to its owner and admins. The owner or an admin edits and deletes
//! a note, and the owner never changes. Admins add and remove members; the
//! first member of an empty organization may add themselves.
//!
//! The `policy` block in `schema!` below is the whole policy: `schema!`
//! generates the `Policy` impl from it (`policy::permits`, `visible`, `stored`
//! and `view`), and the generated Lean schema states what the block means
//! (`notes_kernel.PolicySpec`) and proves the extracted functions compute it.
//! The server would run [`App`], the kernel behind the guard, so the
//! framework's `guarded_transition.authorized` holds for this policy with no
//! hand-written Lean. The handlers below check input and existence only.

use h5i_app_std::authz::{Disclose, Transition, View};

pub type Text = Vec<u8>;

/// The longest note, in bytes.
pub const MAX_TEXT: usize = 4096;

/// Who is calling: an organization (the tenant) and a user in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Principal {
    pub org: u64,
    pub user: u64,
}

h5i_app_schema::schema! {
    mapping notes_tables for notes_kernel, writes Write, lean "../proofs/generated/Schema.lean";

    policy Notes {
        principal Principal.user;
        write Write;
        row Row;
        admin members.admin;
        table Member in members key user: u64 {
            put PutMember del DelMember row OfMember owner user
            write [admin, first] visible [owner, admin]
        }
        table Note in notes key id: u64 {
            put PutNote del DelNote row OfNote owner owner
            write [owner, admin] visible [public, owner, admin]
        }
    }

    /// One organization's state.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Snapshot {
        members: Vec<Member>,
        notes: Vec<Note>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Member in "members" {
        key { user: u64 }
        admin: bool,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Note in "notes" {
        key { id: u64 }
        owner: u64,
        public: bool,
        body: Text,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Add a member, as admin if asked: the policy decides who may.
    AddMember { user: u64, admin: bool }, // h5i-allow: privileged-field (admins, or the first member, by the policy)
    RemoveMember { user: u64 },
    /// Create note `id` as the caller, or replace its body and visibility.
    PutNote { id: u64, body: Text, public: bool },
    DeleteNote { id: u64 },
    ReadNote { id: u64 },
    ListNotes,
    ListMembers,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    PutMember(Member),
    DelMember(u64),
    PutNote(Note),
    DelNote(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Done,
    Found(Note),
    Notes(Vec<Note>),
    Members(Vec<Member>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotFound,
    BadText,
}

/// A row a reply discloses, one variant per table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    OfMember(Member),
    OfNote(Note),
}

type Outcome = Result<(Vec<Write>, Reply), Error>;

fn one(w: Write) -> Vec<Write> {
    let mut ws = Vec::new();
    ws.push(w);
    ws
}

pub fn text_ok(t: &Text) -> bool {
    t.len() > 0 && t.len() <= MAX_TEXT
}

/* ---------------------------------------------------------------------------
 * Handlers: what each command does. No authorization here.
 * ------------------------------------------------------------------------- */

fn put_note(user: u64, s: &Snapshot, id: u64, body: &Text, public: bool) -> Outcome {
    if !text_ok(body) {
        return Err(Error::BadText);
    }
    let owner = match policy::notes::find(&s.notes, id) {
        Some(old) => old.owner,
        None => user,
    };
    Ok((one(Write::PutNote(Note { id, owner, public, body: body.clone() })), Reply::Done))
}

fn delete_note(s: &Snapshot, id: u64) -> Outcome {
    match policy::notes::find(&s.notes, id) {
        None => Err(Error::NotFound),
        Some(_) => Ok((one(Write::DelNote(id)), Reply::Done)),
    }
}

fn remove_member(s: &Snapshot, user: u64) -> Outcome {
    match policy::members::find(&s.members, user) {
        None => Err(Error::NotFound),
        Some(_) => Ok((one(Write::DelMember(user)), Reply::Done)),
    }
}

fn read_note(s: &Snapshot, id: u64) -> Outcome {
    match policy::notes::find(&s.notes, id) {
        None => Err(Error::NotFound),
        Some(n) => Ok((Vec::new(), Reply::Found(n))),
    }
}

/// The app logic: a command's writes and reply, or a refusal that commits
/// nothing. Read-only commands get the caller's view as `s`.
pub fn transition(actor: &Principal, s: &Snapshot, cmd: &Command) -> Outcome {
    match cmd {
        Command::AddMember { user, admin } => Ok((one(Write::PutMember(Member { user: *user, admin: *admin })), Reply::Done)),
        Command::RemoveMember { user } => remove_member(s, *user),
        Command::PutNote { id, body, public } => put_note(actor.user, s, *id, body, *public),
        Command::DeleteNote { id } => delete_note(s, *id),
        Command::ReadNote { id } => read_note(s, *id),
        Command::ListNotes => Ok((Vec::new(), Reply::Notes(s.notes.clone()))),
        Command::ListMembers => Ok((Vec::new(), Reply::Members(s.members.clone()))),
    }
}

/// What one write does to the state; `schema!` builds `apply` from it.
fn apply_write(s: &mut Snapshot, w: Write) {
    match w {
        Write::PutMember(m) => Member::put(&mut s.members, m),
        Write::DelMember(user) => s.members = Member::del(&s.members, user),
        Write::PutNote(n) => Note::put(&mut s.notes, n),
        Write::DelNote(id) => s.notes = Note::del(&s.notes, id),
    }
}

/// The table writes one write makes.
fn sql_write(w: &Write, out: &mut Vec<h5i_app_sql::Write>) {
    match w {
        Write::PutMember(m) => out.push(m.sql_put()),
        Write::DelMember(user) => out.push(Member::sql_del(*user)),
        Write::PutNote(n) => out.push(n.sql_put()),
        Write::DelNote(id) => out.push(Note::sql_del(*id)),
    }
}

/* ---------------------------------------------------------------------------
 * Wiring: the policy is `schema!`'s; these say which rows a reply carries,
 * which commands only read, and that the kernel's `transition` and `apply`
 * are the ones above.
 * ------------------------------------------------------------------------- */

impl Disclose<Row> for Reply {
    /// Every row a reply carries.
    fn disclosed(&self) -> Vec<Row> {
        let mut out = Vec::new();
        match self {
            Reply::Done => {}
            Reply::Found(n) => out.push(Row::OfNote(n.clone())),
            Reply::Notes(ns) => {
                let mut i = 0;
                while i < ns.len() {
                    out.push(Row::OfNote(ns[i].clone()));
                    i += 1;
                }
            }
            Reply::Members(ms) => {
                let mut i = 0;
                while i < ms.len() {
                    out.push(Row::OfMember(ms[i]));
                    i += 1;
                }
            }
        }
        out
    }
}

impl View<Principal, Snapshot, Command> for Notes {
    fn read_only(cmd: &Command) -> bool {
        match cmd {
            Command::ReadNote { .. } | Command::ListNotes | Command::ListMembers => true,
            _ => false,
        }
    }

    /// The rows the policy lets the caller see.
    fn view(actor: &Principal, s: &Snapshot) -> Snapshot {
        policy::view(actor, s)
    }
}

impl Transition<Principal, Snapshot, Command, Write, Reply, Error> for Notes {
    fn kernel_transition(actor: &Principal, s: &Snapshot, cmd: &Command) -> Outcome {
        transition(actor, s, cmd)
    }

    fn kernel_apply(s: &Snapshot, ws: &Vec<Write>) -> Snapshot {
        apply(s, ws)
    }
}

h5i_app_std::authz_kernel! {
    Notes {
        principal: Principal, snapshot: Snapshot, command: Command,
        write: Write, reply: Reply, error: Error, row: Row,
        tenant: |actor| actor.org,
    }
}

#[cfg(test)]
mod tests;
