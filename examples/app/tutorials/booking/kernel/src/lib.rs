//! Tutorial 5: booking meeting rooms.
//!
//! Admins create rooms. Users book a room for a half-open interval
//! `[start_at, end_at)` that starts in the future and overlaps no other
//! booking of the room. Users cancel their own bookings before they start, and
//! admins cancel any booking. Every booking and cancellation notifies the
//! destination registered for the room, through the outbox.
//!
//! Times are Unix seconds. The kernel has no clock; the shell puts the time
//! in each request's `Principal`.

/// Who is calling, and when. `now` is the server's clock, never the request's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Principal {
    pub org: u64,
    pub user: u64,
    pub now: u64,
}

h5i_app_schema::schema! {
    mapping booking_tables for booking_kernel, writes Write, lean "../proofs/generated/Schema.lean";

    /// One organization's state.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Snapshot {
        counter: Counter,
        admins: Vec<Admin>,
        rooms: Vec<Room>,
        bookings: Vec<Booking>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Admin in "admins" {
        key { user: u64 }
    }

    /// `dest` names an entry of the operator's destination registry.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Room in "rooms" {
        key { id: u64 }
        dest: u64,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Booking in "bookings" {
        key { id: u64 }
        room: u64,
        user: u64,
        start_at: u64,
        end_at: u64,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
    pub struct Counter in "counters" {
        key {}
        next_id: u64,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Booked,
    Cancelled,
}

/// A notification for the outbox: what happened to which booking, sent to `dest`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Effect {
    pub dest: u64,
    pub event: Event,
    pub booking: Booking,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    AddAdmin { user: u64 },
    CreateRoom { dest: u64 },
    Book { room: u64, start_at: u64, end_at: u64 },
    Cancel { id: u64 },
    List,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Write {
    PutAdmin(Admin),
    PutRoom(Room),
    PutBooking(Booking),
    DelBooking(u64),
    SetCounter(Counter),
    Emit(Effect),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Created(u64),
    Done,
    Bookings(Vec<Booking>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotFound,
    Forbidden,
    BadInterval,
    InThePast,
    Taken,
    Started,
    Overflow,
}

type Outcome = Result<(Vec<Write>, Reply), Error>;

fn one(w: Write) -> Vec<Write> {
    let mut ws = Vec::new();
    ws.push(w);
    ws
}

pub fn is_admin(v: &Vec<Admin>, user: u64) -> bool {
    let mut i = 0;
    while i < v.len() {
        if v[i].user == user {
            return true;
        }
        i += 1;
    }
    false
}

pub fn find_room(v: &Vec<Room>, id: u64) -> Option<Room> {
    let mut i = 0;
    while i < v.len() {
        if v[i].id == id {
            return Some(v[i]);
        }
        i += 1;
    }
    None
}

pub fn find_booking(v: &Vec<Booking>, id: u64) -> Option<Booking> {
    let mut i = 0;
    while i < v.len() {
        if v[i].id == id {
            return Some(v[i]);
        }
        i += 1;
    }
    None
}

/// No booking of `room` overlaps `[start_at, end_at)`.
pub fn free(v: &Vec<Booking>, room: u64, start_at: u64, end_at: u64) -> bool {
    let mut i = 0;
    while i < v.len() {
        if v[i].room == room && start_at < v[i].end_at && v[i].start_at < end_at {
            return false;
        }
        i += 1;
    }
    true
}

fn add_admin(user: u64, s: &Snapshot, target: u64) -> Outcome {
    let bootstrap = s.admins.len() == 0 && target == user;
    if is_admin(&s.admins, user) || bootstrap {
        Ok((one(Write::PutAdmin(Admin { user: target })), Reply::Done))
    } else {
        Err(Error::Forbidden)
    }
}

fn create_room(user: u64, s: &Snapshot, dest: u64) -> Outcome {
    if !is_admin(&s.admins, user) {
        return Err(Error::Forbidden);
    }
    let id = s.counter.next_id;
    if id == u64::MAX {
        return Err(Error::Overflow);
    }
    let mut ws = Vec::new();
    ws.push(Write::PutRoom(Room { id, dest }));
    ws.push(Write::SetCounter(Counter { next_id: id + 1 }));
    Ok((ws, Reply::Created(id)))
}

fn book(a: &Principal, s: &Snapshot, room: u64, start_at: u64, end_at: u64) -> Outcome {
    let r = match find_room(&s.rooms, room) {
        None => return Err(Error::NotFound),
        Some(r) => r,
    };
    if start_at >= end_at {
        return Err(Error::BadInterval);
    }
    if start_at <= a.now {
        return Err(Error::InThePast);
    }
    if !free(&s.bookings, room, start_at, end_at) {
        return Err(Error::Taken);
    }
    let id = s.counter.next_id;
    if id == u64::MAX {
        return Err(Error::Overflow);
    }
    let b = Booking { id, room, user: a.user, start_at, end_at };
    let mut ws = Vec::new();
    ws.push(Write::PutBooking(b));
    ws.push(Write::SetCounter(Counter { next_id: id + 1 }));
    ws.push(Write::Emit(Effect { dest: r.dest, event: Event::Booked, booking: b }));
    Ok((ws, Reply::Created(id)))
}

fn cancel(a: &Principal, s: &Snapshot, id: u64) -> Outcome {
    let b = match find_booking(&s.bookings, id) {
        None => return Err(Error::NotFound),
        Some(b) => b,
    };
    if !is_admin(&s.admins, a.user) {
        if b.user != a.user {
            return Err(Error::Forbidden);
        }
        if b.start_at <= a.now {
            return Err(Error::Started);
        }
    }
    let r = match find_room(&s.rooms, b.room) {
        None => return Err(Error::NotFound),
        Some(r) => r,
    };
    let mut ws = Vec::new();
    ws.push(Write::DelBooking(id));
    ws.push(Write::Emit(Effect { dest: r.dest, event: Event::Cancelled, booking: b }));
    Ok((ws, Reply::Done))
}

/// The app logic: a command's writes and reply, or a refusal that commits
/// and sends nothing.
pub fn transition(actor: &Principal, s: &Snapshot, cmd: &Command) -> Outcome {
    match cmd {
        Command::AddAdmin { user } => add_admin(actor.user, s, *user),
        Command::CreateRoom { dest } => create_room(actor.user, s, *dest),
        Command::Book { room, start_at, end_at } => book(actor, s, *room, *start_at, *end_at),
        Command::Cancel { id } => cancel(actor, s, *id),
        Command::List => Ok((Vec::new(), Reply::Bookings(s.bookings.clone()))),
    }
}

/// What one write does to the state; `schema!` builds `apply` from it.
fn apply_write(s: &mut Snapshot, w: Write) {
    match w {
        Write::PutAdmin(x) => Admin::put(&mut s.admins, x),
        Write::PutRoom(x) => Room::put(&mut s.rooms, x),
        Write::PutBooking(x) => Booking::put(&mut s.bookings, x),
        Write::DelBooking(id) => s.bookings = Booking::del(&s.bookings, id),
        Write::SetCounter(c) => s.counter = c,
        // Effects leave through the outbox; they change no table.
        Write::Emit(_) => {}
    }
}

/// The table writes one write makes; an effect makes none.
fn sql_write(w: &Write, out: &mut Vec<h5i_app_sql::Write>) {
    match w {
        Write::PutAdmin(x) => out.push(x.sql_put()),
        Write::PutRoom(x) => out.push(x.sql_put()),
        Write::PutBooking(x) => out.push(x.sql_put()),
        Write::DelBooking(id) => out.push(Booking::sql_del(*id)),
        Write::SetCounter(c) => out.push(c.sql_put()),
        Write::Emit(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &Snapshot, user: u64, now: u64, c: Command) -> (Snapshot, Result<(Vec<Write>, Reply), Error>) {
        match transition(&Principal { org: 1, user, now }, s, &c) {
            Ok((ws, r)) => (apply(s, &ws), Ok((ws, r))),
            Err(e) => (s.clone(), Err(e)),
        }
    }

    fn reply(r: Result<(Vec<Write>, Reply), Error>) -> Result<Reply, Error> {
        r.map(|(_, r)| r)
    }

    #[test]
    fn book_and_cancel() {
        let s = Snapshot::default();
        let (s, _) = run(&s, 1, 0, Command::AddAdmin { user: 1 });
        assert_eq!(reply(run(&s, 2, 0, Command::CreateRoom { dest: 7 }).1), Err(Error::Forbidden));
        let (s, r) = run(&s, 1, 0, Command::CreateRoom { dest: 7 });
        assert_eq!(reply(r), Ok(Reply::Created(0)));

        let (s, r) = run(&s, 2, 0, Command::Book { room: 0, start_at: 1, end_at: 2 });
        let (ws, rep) = r.unwrap();
        assert_eq!(rep, Reply::Created(1));
        let b = Booking { id: 1, room: 0, user: 2, start_at: 1, end_at: 2 };
        assert_eq!(ws[2], Write::Emit(Effect { dest: 7, event: Event::Booked, booking: b }));

        // Adjacent intervals do not overlap; [1, 3) does.
        let (s, r) = run(&s, 3, 0, Command::Book { room: 0, start_at: 2, end_at: 3 });
        assert_eq!(reply(r), Ok(Reply::Created(2)));
        assert_eq!(reply(run(&s, 3, 0, Command::Book { room: 0, start_at: 1, end_at: 3 }).1), Err(Error::Taken));
        assert_eq!(reply(run(&s, 3, 5, Command::Book { room: 0, start_at: 5, end_at: 6 }).1), Err(Error::InThePast));
        assert_eq!(reply(run(&s, 3, 0, Command::Book { room: 0, start_at: 4, end_at: 4 }).1), Err(Error::BadInterval));

        // Only the owner cancels, and only before the start; admins always can.
        assert_eq!(reply(run(&s, 3, 0, Command::Cancel { id: 1 }).1), Err(Error::Forbidden));
        assert_eq!(reply(run(&s, 2, 1, Command::Cancel { id: 1 }).1), Err(Error::Started));
        let (t, r) = run(&s, 2, 0, Command::Cancel { id: 1 });
        assert_eq!(r.unwrap().0[1], Write::Emit(Effect { dest: 7, event: Event::Cancelled, booking: b }));
        assert_eq!(t.bookings.len(), 1);
        let (t, r) = run(&s, 1, 9, Command::Cancel { id: 2 });
        assert_eq!(reply(r), Ok(Reply::Done));
        assert_eq!(t.bookings, vec![b]);
    }
}
