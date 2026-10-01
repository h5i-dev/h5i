//! Tutorial 2: a bulletin board with authors and moderators.
//!
//! Everyone in an organization can read and publish posts. Only the author
//! may edit a post, and the author or a moderator may delete it. Moderators
//! appoint and remove moderators; when there is none yet, a user may appoint
//! themselves, and the last moderator cannot be removed.

pub type Text = Vec<u8>;

/// The longest post, in bytes.
pub const MAX_TEXT: usize = 280;

/// Who is calling: an organization (the tenant) and a user in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Principal {
    pub org: u64,
    pub user: u64,
}

h5i_app_schema::schema! {
    mapping board_tables for board_kernel, writes Write, lean "../proofs/generated/Schema.lean";

    /// One organization's state.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Snapshot {
        counter: Counter,
        posts: Vec<Post>,
        moderators: Vec<Moderator>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Post in "posts" {
        key { id: u64 }
        author: u64,
        text: Text,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Moderator in "moderators" {
        key { user: u64 }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
    pub struct Counter in "counters" {
        key {}
        next_id: u64,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Publish { text: Text },
    Edit { id: u64, text: Text },
    Delete { id: u64 },
    List,
    Promote { user: u64 },
    Demote { user: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    PutPost(Post),
    DelPost(u64),
    PutModerator(Moderator),
    DelModerator(u64),
    SetCounter(Counter),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Created(u64),
    Done,
    Posts(Vec<Post>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotFound,
    Forbidden,
    BadText,
    LastModerator,
    Overflow,
}

type Outcome = Result<(Vec<Write>, Reply), Error>;

fn one(w: Write) -> Vec<Write> {
    let mut ws = Vec::new();
    ws.push(w);
    ws
}

/// A post must be non-empty and at most `MAX_TEXT` bytes.
pub fn text_ok(t: &Text) -> bool {
    t.len() > 0 && t.len() <= MAX_TEXT
}

pub fn find_post(ps: &Vec<Post>, id: u64) -> Option<Post> {
    let mut i = 0;
    while i < ps.len() {
        if ps[i].id == id {
            return Some(ps[i].clone());
        }
        i += 1;
    }
    None
}

pub fn is_moderator(ms: &Vec<Moderator>, user: u64) -> bool {
    let mut i = 0;
    while i < ms.len() {
        if ms[i].user == user {
            return true;
        }
        i += 1;
    }
    false
}

fn publish(user: u64, s: &Snapshot, text: &Text) -> Outcome {
    if !text_ok(text) {
        return Err(Error::BadText);
    }
    let id = s.counter.next_id;
    if id == u64::MAX {
        return Err(Error::Overflow);
    }
    let mut ws = Vec::new();
    ws.push(Write::PutPost(Post { id, author: user, text: text.clone() }));
    ws.push(Write::SetCounter(Counter { next_id: id + 1 }));
    Ok((ws, Reply::Created(id)))
}

fn edit(user: u64, s: &Snapshot, id: u64, text: &Text) -> Outcome {
    match find_post(&s.posts, id) {
        None => Err(Error::NotFound),
        Some(p) => {
            if p.author != user {
                Err(Error::Forbidden)
            } else if !text_ok(text) {
                Err(Error::BadText)
            } else {
                Ok((one(Write::PutPost(Post { id, author: p.author, text: text.clone() })), Reply::Done))
            }
        }
    }
}

fn delete(user: u64, s: &Snapshot, id: u64) -> Outcome {
    match find_post(&s.posts, id) {
        None => Err(Error::NotFound),
        Some(p) => {
            if p.author == user || is_moderator(&s.moderators, user) {
                Ok((one(Write::DelPost(id)), Reply::Done))
            } else {
                Err(Error::Forbidden)
            }
        }
    }
}

fn promote(user: u64, s: &Snapshot, target: u64) -> Outcome {
    let bootstrap = s.moderators.len() == 0 && target == user;
    if is_moderator(&s.moderators, user) || bootstrap {
        Ok((one(Write::PutModerator(Moderator { user: target })), Reply::Done))
    } else {
        Err(Error::Forbidden)
    }
}

fn demote(user: u64, s: &Snapshot, target: u64) -> Outcome {
    if !is_moderator(&s.moderators, user) {
        Err(Error::Forbidden)
    } else if !is_moderator(&s.moderators, target) {
        Err(Error::NotFound)
    } else if s.moderators.len() <= 1 {
        Err(Error::LastModerator)
    } else {
        Ok((one(Write::DelModerator(target)), Reply::Done))
    }
}

/// The app logic: a command's writes and reply, or a refusal that commits
/// nothing.
pub fn transition(actor: &Principal, s: &Snapshot, cmd: &Command) -> Outcome {
    match cmd {
        Command::Publish { text } => publish(actor.user, s, text),
        Command::Edit { id, text } => edit(actor.user, s, *id, text),
        Command::Delete { id } => delete(actor.user, s, *id),
        Command::List => Ok((Vec::new(), Reply::Posts(s.posts.clone()))),
        Command::Promote { user } => promote(actor.user, s, *user),
        Command::Demote { user } => demote(actor.user, s, *user),
    }
}

/// What one write does to the state; `schema!` builds `apply` from it.
fn apply_write(s: &mut Snapshot, w: Write) {
    match w {
        Write::PutPost(p) => Post::put(&mut s.posts, p),
        Write::DelPost(id) => s.posts = Post::del(&s.posts, id),
        Write::PutModerator(m) => Moderator::put(&mut s.moderators, m),
        Write::DelModerator(u) => s.moderators = Moderator::del(&s.moderators, u),
        Write::SetCounter(c) => s.counter = c,
    }
}

/// The table writes one write makes; `Storage.lean` proves they store what
/// `apply` computes.
fn sql_write(w: &Write, out: &mut Vec<h5i_app_sql::Write>) {
    match w {
        Write::PutPost(p) => out.push(p.sql_put()),
        Write::DelPost(id) => out.push(Post::sql_del(*id)),
        Write::PutModerator(m) => out.push(m.sql_put()),
        Write::DelModerator(u) => out.push(Moderator::sql_del(*u)),
        Write::SetCounter(c) => out.push(c.sql_put()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &Snapshot, user: u64, c: Command) -> (Snapshot, Result<Reply, Error>) {
        match transition(&Principal { org: 1, user }, s, &c) {
            Ok((ws, r)) => (apply(s, &ws), Ok(r)),
            Err(e) => (s.clone(), Err(e)),
        }
    }

    #[test]
    fn authors_and_moderators() {
        let s = Snapshot::default();
        let (s, r) = run(&s, 1, Command::Publish { text: b"hello".to_vec() });
        assert_eq!(r, Ok(Reply::Created(0)));
        assert_eq!(run(&s, 2, Command::Edit { id: 0, text: b"mine".to_vec() }).1, Err(Error::Forbidden));
        assert_eq!(run(&s, 2, Command::Delete { id: 0 }).1, Err(Error::Forbidden));
        let (s, _) = run(&s, 2, Command::Promote { user: 2 });
        assert_eq!(run(&s, 3, Command::Promote { user: 3 }).1, Err(Error::Forbidden));
        assert_eq!(run(&s, 2, Command::Demote { user: 2 }).1, Err(Error::LastModerator));
        let (s, r) = run(&s, 2, Command::Delete { id: 0 });
        assert_eq!(r, Ok(Reply::Done));
        assert!(s.posts.is_empty());
    }
}
