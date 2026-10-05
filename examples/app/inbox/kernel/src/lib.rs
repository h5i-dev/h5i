//! Tutorial 4: private messages inside an organization.
//!
//! Users send messages to each other, read their inbox and sent box, mark
//! messages read, delete them from their own view and block senders. A user
//! only ever sees the messages they sent or received.

pub type Text = Vec<u8>;

/// The longest message, in bytes.
pub const MAX_TEXT: usize = 1000;

/// Who is calling: an organization (the tenant) and a user in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Principal {
    pub org: u64,
    pub user: u64,
}

h5i_app_schema::schema! {
    mapping inbox_tables for inbox_kernel, writes Write, lean "../proofs/generated/Schema.lean";

    /// One organization's state.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Snapshot {
        messages: Vec<Message>,
        blocks: Vec<Block>,
    }

    /// The `seq`-th message from `sender` to `recipient`. Each side hides it
    /// from their own view with a flag; the row stays for the other side.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Message in "messages" {
        key { sender: u64, recipient: u64, seq: u64 }
        text: Text,
        read: bool,
        sender_deleted: bool,
        recipient_deleted: bool,
    }

    /// `owner` refuses messages from `sender`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Block in "blocks" {
        key { owner: u64, sender: u64 }
    }
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Send { to: u64, text: Text },
    Inbox,
    Sent,
    MarkRead { from: u64, seq: u64 },
    Delete { from: u64, to: u64, seq: u64 },
    Block { user: u64 },
    Unblock { user: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    PutMessage(Message),
    PutBlock(Block),
    DelBlock(Block),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Sent(u64),
    Done,
    Messages(Vec<Message>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotFound,
    Blocked,
    BadText,
    Overflow,
}

type Outcome = Result<(Vec<Write>, Reply), Error>;

fn one(w: Write) -> Vec<Write> {
    let mut ws = Vec::new();
    ws.push(w);
    ws
}

/// A message is non-empty and at most `MAX_TEXT` bytes.
pub fn text_ok(t: &Text) -> bool {
    t.len() > 0 && t.len() <= MAX_TEXT
}

pub fn is_blocked(bs: &Vec<Block>, owner: u64, sender: u64) -> bool {
    let mut i = 0;
    while i < bs.len() {
        if bs[i].owner == owner && bs[i].sender == sender {
            return true;
        }
        i += 1;
    }
    false
}

/// The largest `seq` from `from` to `to` so far, if any.
pub fn last_seq(ms: &Vec<Message>, from: u64, to: u64) -> Option<u64> {
    let mut last = None;
    let mut i = 0;
    while i < ms.len() {
        if ms[i].sender == from && ms[i].recipient == to {
            last = match last {
                None => Some(ms[i].seq),
                Some(k) => {
                    if ms[i].seq > k {
                        Some(ms[i].seq)
                    } else {
                        Some(k)
                    }
                }
            };
        }
        i += 1;
    }
    last
}

pub fn find_message(ms: &Vec<Message>, from: u64, to: u64, seq: u64) -> Option<Message> {
    let mut i = 0;
    while i < ms.len() {
        if ms[i].sender == from && ms[i].recipient == to && ms[i].seq == seq {
            return Some(ms[i].clone());
        }
        i += 1;
    }
    None
}

/// The messages `user` received (`incoming`) or sent, minus those they deleted.
pub fn mailbox(ms: &Vec<Message>, user: u64, incoming: bool) -> Vec<Message> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < ms.len() {
        let keep = if incoming {
            ms[i].recipient == user && !ms[i].recipient_deleted
        } else {
            ms[i].sender == user && !ms[i].sender_deleted
        };
        if keep {
            out.push(ms[i].clone());
        }
        i += 1;
    }
    out
}

fn send(user: u64, s: &Snapshot, to: u64, text: &Text) -> Outcome {
    if !text_ok(text) {
        return Err(Error::BadText);
    }
    // The sender learns that `to` blocked them; `view` in Spec.lean says so.
    if is_blocked(&s.blocks, to, user) {
        return Err(Error::Blocked);
    }
    let seq = match last_seq(&s.messages, user, to) {
        None => 0,
        Some(k) => {
            if k == u64::MAX {
                return Err(Error::Overflow);
            }
            k + 1
        }
    };
    let m = Message {
        sender: user,
        recipient: to,
        seq,
        text: text.clone(),
        read: false,
        sender_deleted: false,
        recipient_deleted: false,
    };
    Ok((one(Write::PutMessage(m)), Reply::Sent(seq)))
}

fn mark_read(user: u64, s: &Snapshot, from: u64, seq: u64) -> Outcome {
    match find_message(&s.messages, from, user, seq) {
        None => Err(Error::NotFound),
        Some(m) => {
            if m.recipient_deleted {
                Err(Error::NotFound)
            } else {
                Ok((one(Write::PutMessage(Message { read: true, ..m })), Reply::Done))
            }
        }
    }
}

fn delete(user: u64, s: &Snapshot, from: u64, to: u64, seq: u64) -> Outcome {
    // Someone else's message is reported exactly like a missing one.
    if from != user && to != user {
        return Err(Error::NotFound);
    }
    match find_message(&s.messages, from, to, seq) {
        None => Err(Error::NotFound),
        Some(m) => {
            let sender_deleted = if from == user { true } else { m.sender_deleted };
            let recipient_deleted = if to == user { true } else { m.recipient_deleted };
            if sender_deleted == m.sender_deleted && recipient_deleted == m.recipient_deleted {
                // Already gone from the caller's view.
                Err(Error::NotFound)
            } else {
                let w = Write::PutMessage(Message { sender_deleted, recipient_deleted, ..m });
                Ok((one(w), Reply::Done))
            }
        }
    }
}

/// The app logic: a command's writes and reply, or a refusal that commits
/// nothing.
pub fn transition(actor: &Principal, s: &Snapshot, cmd: &Command) -> Outcome {
    let user = actor.user;
    match cmd {
        Command::Send { to, text } => send(user, s, *to, text),
        Command::Inbox => Ok((Vec::new(), Reply::Messages(mailbox(&s.messages, user, true)))),
        Command::Sent => Ok((Vec::new(), Reply::Messages(mailbox(&s.messages, user, false)))),
        Command::MarkRead { from, seq } => mark_read(user, s, *from, *seq),
        Command::Delete { from, to, seq } => delete(user, s, *from, *to, *seq),
        Command::Block { user: u } => Ok((one(Write::PutBlock(Block { owner: user, sender: *u })), Reply::Done)),
        Command::Unblock { user: u } => Ok((one(Write::DelBlock(Block { owner: user, sender: *u })), Reply::Done)),
    }
}

/// What one write does to the state; `schema!` builds `apply` from it.
fn apply_write(s: &mut Snapshot, w: Write) {
    match w {
        Write::PutMessage(m) => Message::put(&mut s.messages, m),
        Write::PutBlock(b) => Block::put(&mut s.blocks, b),
        Write::DelBlock(b) => s.blocks = Block::del(&s.blocks, b.owner, b.sender),
    }
}

/// The table writes one write makes.
fn sql_write(w: &Write, out: &mut Vec<h5i_app_sql::Write>) {
    match w {
        Write::PutMessage(m) => out.push(m.sql_put()),
        Write::PutBlock(b) => out.push(b.sql_put()),
        Write::DelBlock(b) => out.push(Block::sql_del(b.owner, b.sender)),
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

    fn texts(r: Result<Reply, Error>) -> Vec<Vec<u8>> {
        match r {
            Ok(Reply::Messages(ms)) => ms.into_iter().map(|m| m.text).collect(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn send_read_delete() {
        let s = Snapshot::default();
        let (s, r) = run(&s, 1, Command::Send { to: 2, text: b"hi bob".to_vec() });
        assert_eq!(r, Ok(Reply::Sent(0)));
        let (s, r) = run(&s, 1, Command::Send { to: 2, text: b"again".to_vec() });
        assert_eq!(r, Ok(Reply::Sent(1)));
        assert_eq!(texts(run(&s, 2, Command::Inbox).1), vec![b"hi bob".to_vec(), b"again".to_vec()]);
        assert!(texts(run(&s, 3, Command::Inbox).1).is_empty());
        // Carol cannot tell Alice's message from a missing one.
        assert_eq!(run(&s, 3, Command::Delete { from: 1, to: 2, seq: 0 }).1, Err(Error::NotFound));
        assert_eq!(run(&s, 3, Command::Delete { from: 1, to: 2, seq: 9 }).1, Err(Error::NotFound));
        let (s, r) = run(&s, 2, Command::MarkRead { from: 1, seq: 0 });
        assert_eq!(r, Ok(Reply::Done));
        let (s, _) = run(&s, 2, Command::Delete { from: 1, to: 2, seq: 0 });
        assert_eq!(texts(run(&s, 2, Command::Inbox).1), vec![b"again".to_vec()]);
        assert_eq!(texts(run(&s, 1, Command::Sent).1).len(), 2);
        assert_eq!(run(&s, 2, Command::Delete { from: 1, to: 2, seq: 0 }).1, Err(Error::NotFound));
    }

    #[test]
    fn blocking() {
        let s = Snapshot::default();
        let (s, _) = run(&s, 2, Command::Block { user: 1 });
        assert_eq!(run(&s, 1, Command::Send { to: 2, text: b"hi".to_vec() }).1, Err(Error::Blocked));
        assert_eq!(run(&s, 1, Command::Send { to: 3, text: b"hi".to_vec() }).1, Ok(Reply::Sent(0)));
        let (s, _) = run(&s, 2, Command::Unblock { user: 1 });
        assert_eq!(run(&s, 1, Command::Send { to: 2, text: b"hi".to_vec() }).1, Ok(Reply::Sent(0)));
    }
}
