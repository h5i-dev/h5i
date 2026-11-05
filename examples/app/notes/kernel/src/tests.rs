use super::*;
use h5i_app_std::authz::{Guarded, Policy};

fn who(user: u64) -> Principal {
    Principal { org: 1, user }
}

fn run(s: &Snapshot, user: u64, c: Command) -> (Snapshot, Result<Reply, Guarded<Error>>) {
    match guarded_transition(&who(user), s, &c) {
        Ok((ws, r)) => (apply(s, &ws), Ok(r)),
        Err(e) => (s.clone(), Err(e)),
    }
}

fn note(id: u64, body: &str, public: bool) -> Command {
    Command::PutNote { id, body: body.as_bytes().to_vec(), public }
}

#[test]
fn members_notes_and_admins() {
    let s = Snapshot::default();
    let (s, r) = run(&s, 1, Command::AddMember { user: 1, admin: true });
    assert_eq!(r, Ok(Reply::Done), "the first member of an empty organization");
    assert_eq!(run(&s, 2, Command::AddMember { user: 2, admin: true }).1, Err(Guarded::Denied), "only admins add members");
    let (s, r) = run(&s, 1, Command::AddMember { user: 2, admin: false });
    assert_eq!(r, Ok(Reply::Done));
    let (s, _) = run(&s, 1, Command::AddMember { user: 3, admin: false });

    let (s, r) = run(&s, 2, note(10, "draft", false));
    assert_eq!(r, Ok(Reply::Done));
    let (s, _) = run(&s, 2, note(11, "public", true));

    // A private note is invisible to others, as if it did not exist; admins see it.
    assert_eq!(run(&s, 3, Command::ReadNote { id: 10 }).1, Err(Guarded::App(Error::NotFound)));
    assert!(matches!(run(&s, 1, Command::ReadNote { id: 10 }).1, Ok(Reply::Found(_))));
    match run(&s, 3, Command::ListNotes).1 {
        Ok(Reply::Notes(ns)) => assert_eq!(ns.len(), 1),
        r => panic!("{r:?}"),
    }

    // The owner or an admin edits and deletes; the owner stays.
    assert_eq!(run(&s, 3, note(10, "mine now", true)).1, Err(Guarded::Denied));
    assert_eq!(run(&s, 1, note(10, "edited by an admin", false)).1, Ok(Reply::Done));
    assert_eq!(run(&s, 3, Command::DeleteNote { id: 11 }).1, Err(Guarded::Denied));
    let (s, r) = run(&s, 2, Command::DeleteNote { id: 11 });
    assert_eq!(r, Ok(Reply::Done));
    assert_eq!(policy::notes::find(&s.notes, 11), None);

    // Members see only their own member row; admins see every one.
    match run(&s, 3, Command::ListMembers).1 {
        Ok(Reply::Members(ms)) => assert_eq!(ms, vec![Member { user: 3, admin: false }]),
        r => panic!("{r:?}"),
    }
    assert_eq!(run(&s, 3, Command::RemoveMember { user: 2 }).1, Err(Guarded::Denied));
    assert_eq!(run(&s, 1, Command::RemoveMember { user: 2 }).1, Ok(Reply::Done));
}

/// The policy as one would write it by hand, in the style of the forum's:
/// the reference the generated one is compared with.
struct Hand;

fn hand_admin(s: &Snapshot, user: u64) -> bool {
    s.members.iter().any(|m| m.user == user && m.admin)
}

impl Policy<Principal, Snapshot, Write, Row> for Hand {
    fn permits(a: &Principal, s: &Snapshot, w: &Write) -> bool {
        let admin = hand_admin(s, a.user);
        match w {
            Write::PutMember(_) => admin || s.members.is_empty(),
            Write::DelMember(user) => s.members.iter().any(|m| m.user == *user) && admin,
            Write::PutNote(n) => match s.notes.iter().find(|o| o.id == n.id) {
                None => n.owner == a.user || admin,
                Some(old) => n.owner == old.owner && (old.owner == a.user || admin),
            },
            Write::DelNote(id) => match s.notes.iter().find(|o| o.id == *id) {
                None => false,
                Some(old) => old.owner == a.user || admin,
            },
        }
    }

    fn visible(a: &Principal, s: &Snapshot, d: &Row) -> bool {
        let admin = hand_admin(s, a.user);
        match d {
            Row::OfMember(m) => m.user == a.user || admin,
            Row::OfNote(n) => n.public || n.owner == a.user || admin,
        }
    }

    fn stored(s: &Snapshot, d: &Row) -> bool {
        match d {
            Row::OfMember(m) => s.members.iter().find(|o| o.user == m.user) == Some(m),
            Row::OfNote(n) => s.notes.iter().find(|o| o.id == n.id) == Some(n),
        }
    }
}

/// Every state over users 1..=3 and note ids 0..=1 (each user absent, a
/// member or an admin; each note absent or owned by one of them, public or
/// not), every write and row over the same values.
fn states() -> Vec<Snapshot> {
    let mut members: Vec<Vec<Member>> = vec![Vec::new()];
    for user in 1..=3u64 {
        let mut next = Vec::new();
        for ms in &members {
            next.push(ms.clone());
            for admin in [false, true] {
                let mut m = ms.clone();
                m.push(Member { user, admin });
                next.push(m);
            }
        }
        members = next;
    }
    let mut notes: Vec<Vec<Note>> = vec![Vec::new()];
    for id in 0..=1u64 {
        let mut next = Vec::new();
        for ns in &notes {
            next.push(ns.clone());
            for owner in 1..=3u64 {
                for public in [false, true] {
                    let mut n = ns.clone();
                    n.push(Note { id, owner, public, body: b"b".to_vec() });
                    next.push(n);
                }
            }
        }
        notes = next;
    }
    let mut out = Vec::new();
    for ms in &members {
        for ns in &notes {
            out.push(Snapshot { members: ms.clone(), notes: ns.clone() });
        }
    }
    out
}

fn writes_and_rows() -> (Vec<Write>, Vec<Row>) {
    let (mut ws, mut rs) = (Vec::new(), Vec::new());
    for user in 1..=3u64 {
        ws.push(Write::DelMember(user));
        for admin in [false, true] {
            ws.push(Write::PutMember(Member { user, admin }));
            rs.push(Row::OfMember(Member { user, admin }));
        }
    }
    for id in 0..=1u64 {
        ws.push(Write::DelNote(id));
        for owner in 1..=3u64 {
            for public in [false, true] {
                for body in [b"b".to_vec(), b"c".to_vec()] {
                    let n = Note { id, owner, public, body };
                    ws.push(Write::PutNote(n.clone()));
                    rs.push(Row::OfNote(n));
                }
            }
        }
    }
    (ws, rs)
}

/// The generated policy equals the hand-written one on every state, actor,
/// write and row of the small domain: 1,323 states, 4 actors, 35 writes and
/// 30 rows.
#[test]
fn generated_policy_equals_the_hand_written_one() {
    let (ws, rs) = writes_and_rows();
    let mut checked = 0u64;
    for s in states() {
        for user in 1..=4u64 {
            let a = who(user);
            for w in &ws {
                assert_eq!(<Notes as Policy<_, _, _, _>>::permits(&a, &s, w), Hand::permits(&a, &s, w), "{a:?} {w:?} {s:?}");
                checked += 1;
            }
            for d in &rs {
                assert_eq!(<Notes as Policy<_, _, _, _>>::visible(&a, &s, d), Hand::visible(&a, &s, d), "{a:?} {d:?} {s:?}");
                assert_eq!(<Notes as Policy<_, _, _, _>>::stored(&s, d), Hand::stored(&s, d), "{d:?} {s:?}");
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 1_323 * 4 * (35 + 30));
}

/// The generated view keeps exactly the rows `visible` accepts.
#[test]
fn the_view_is_the_visible_rows() {
    for s in states().iter().step_by(7) {
        for user in 1..=4u64 {
            let a = who(user);
            let v = policy::view(&a, s);
            let ms: Vec<Member> = s.members.iter().filter(|m| Hand::visible(&a, s, &Row::OfMember(**m))).copied().collect();
            let ns: Vec<Note> = s.notes.iter().filter(|n| Hand::visible(&a, s, &Row::OfNote((*n).clone()))).cloned().collect();
            assert_eq!(v, Snapshot { members: ms, notes: ns });
        }
    }
}

#[test]
fn read_only_matches_the_view() {
    for c in [Command::ListNotes, Command::ReadNote { id: 0 }, Command::ListMembers] {
        assert!(read_only(&c));
    }
    assert!(!read_only(&Command::DeleteNote { id: 0 }));
}
