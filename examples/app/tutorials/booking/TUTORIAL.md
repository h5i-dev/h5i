# Tutorial 5: booking meeting rooms

In this tutorial you build a service for booking meeting rooms. Admins create
rooms, and users book a room for an interval of time. A booking must start in
the future and may not overlap another booking of the same room. Users cancel
their own bookings before they start, and admins cancel any booking. Each
booking and each cancellation sends a notification to a destination that the
admin registered for the room, such as a display next to the door.

The application builds on the [second tutorial](../board/TUTORIAL.md), whose
permission policy, invariants and `Reachable` states you will use again. It
adds three things. The kernel has no clock, so the engine has to pass it the
current time, and you have to decide how much to trust it. The invariant is
about intervals: no two bookings of a room overlap, in every state the service
can reach. And a command now has effects outside the database, which go
through the outbox, and you will prove that each one goes to the right place
and only when its change is committed.

| File | Contents |
|---|---|
| `kernel/src/lib.rs` | the application logic |
| `server/src/lib.rs`, `server/src/main.rs` | the store, the JSON API, the notifier and the axum server |
| `server/tests/postgres.rs`, `server/tests/outbox.rs` | the store and the outbox, tested against PostgreSQL |
| `proofs/Spec.lean` | the policy, the meaning of writes and the invariants |
| `proofs/Commands.lean` | what each command writes, and when it succeeds |
| `proofs/Theorems.lean` | the permission, invariant and notification theorems |
| `proofs/Apply.lean` | the proof that committing a write set does what the specification says |
| `proofs/Scenarios.lean` | concrete runs of the extracted code |
| `proofs/Clock.lean` | a theorem that needs time to move forward, and a run where it does not |

## Running the application

With PostgreSQL running as in the first tutorial, create tokens for an admin
and two users, and start the server. `BOOKING_DESTINATIONS` is the operator's
list of notification destinations; here destination 7 writes to the log.

```
cd examples  # its own Cargo workspace; run from the repository root
export DATABASE_URL=postgres://h5i:h5i@127.0.0.1:55432/h5i H5I_APP_SECRET=dev-secret
ADMIN=$(H5I_APP_ISSUE=5:1 cargo run -q -p booking-server)
ALICE=$(H5I_APP_ISSUE=5:2 cargo run -q -p booking-server)
BOB=$(H5I_APP_ISSUE=5:3 cargo run -q -p booking-server)
BOOKING_DESTINATIONS=7=log cargo run -p booking-server &

rpc() { curl -s -H "Authorization: Bearer $1" -H 'content-type: application/json' -d "$2" localhost:8080/rpc; echo; }
```

The first admin appoints themselves, as the first moderator did on the board,
and creates a room that notifies destination 7:

```
rpc $ADMIN '{"cmd":"add_admin","user":1}'      # {"ok":true}
rpc $ALICE '{"cmd":"create_room","dest":7}'    # {"error":"forbidden"}
rpc $ADMIN '{"cmd":"create_room","dest":7}'    # {"id":0}
```

Times are Unix seconds. Alice books tomorrow from 9 to 10. Bob cannot have
9:30 to 10:30, but 10 to 11 is free, because an interval `[start, end)`
includes its start and not its end. Nobody can book the past, and the time
cannot come from the request:

```
T=$(date -d 'tomorrow 09:00' +%s)
rpc $ALICE '{"cmd":"book","room":0,"start":'$T',"end":'$((T+3600))'}'           # {"id":1}
rpc $BOB   '{"cmd":"book","room":0,"start":'$((T+1800))',"end":'$((T+5400))'}'  # {"error":"taken"}
rpc $BOB   '{"cmd":"book","room":0,"start":'$((T+3600))',"end":'$((T+7200))'}'  # {"id":2}
rpc $BOB   '{"cmd":"book","room":0,"start":1700000000,"end":1700003600}'        # {"error":"in_the_past"}
rpc $BOB   '{"cmd":"book","room":0,"start":'$T',"end":'$T',"now":0}'
# {"error":"unknown field `now`, expected one of `room`, `start`, `end`"}
```

Bob cannot cancel Alice's booking, but she can:

```
rpc $BOB   '{"cmd":"cancel","id":1}'           # {"error":"forbidden"}
rpc $ALICE '{"cmd":"cancel","id":1}'           # {"ok":true}
rpc $ADMIN '{"cmd":"list"}'
# [{"id":2,"room":0,"user":3,"start":1790604000,"end":1790607600}]
```

Within a second, the server's log shows the three notifications, one for each
successful booking or cancellation and none for the refusals:

```
INFO booking_server: notification key=5-1 dest=7 attempt=1 body={"event":"booked","booking":1,"room":0,"user":2,"start":1790600400,"end":1790604000}
INFO booking_server: notification key=5-2 dest=7 attempt=1 body={"event":"booked","booking":2,"room":0,"user":3,"start":1790604000,"end":1790607600}
INFO booking_server: notification key=5-3 dest=7 attempt=1 body={"event":"cancelled","booking":1,"room":0,"user":2,"start":1790600400,"end":1790604000}
```

## The kernel

### Time as an input

`transition` is a pure function, so it cannot read a clock, and yet `Book` has
to know whether a booking is in the future. The current time is therefore an
input, and the question is which input. It should not be a field of the
command, because the command is what the client sends, and a client that
chooses the time can book the past. Instead, the time belongs to the request,
next to the caller:

```rust
pub struct Principal {
    pub org: u64,
    pub user: u64,
    pub now: u64,
}
```

The engine fills in `now`, and the JSON decoder has no field for it. Each
attempt reads the engine's clock inside its transaction and passes the time
to `Kernel::stamp`, which copies it into the principal right before
`transition`. A retry after a serialization failure reads the clock again, so
the decision that commits is always made at the time of the attempt that
commits it. An idempotent retry that arrives later still gets the stored
reply, because the reply is matched on the command alone.

### Rooms and bookings

A room stores the id of its notification destination, and a booking stores
its interval:

```rust
pub struct Room in "rooms" {
    key { id: u64 }
    dest: u64,
}

pub struct Booking in "bookings" {
    key { id: u64 }
    room: u64,
    user: u64,
    start_at: u64,
    end_at: u64,
}
```

`dest` is a number, not a URL. Only the operator's registry turns it into an
endpoint, so an admin cannot make the server call a host of their choosing.

Two half-open intervals `[s₁, e₁)` and `[s₂, e₂)` overlap when each starts
before the other ends. `free` checks the new interval against every booking of
the room:

```rust
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
```

### Effects

A notification is a write like any other. `Book` returns the booking, the new
counter and an `Emit` of an `Effect`, which names the destination registered
for the room, what happened and the booking it happened to:

```rust
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
    // ... next id, overflow check
    let b = Booking { id, room, user: a.user, start_at, end_at };
    let mut ws = Vec::new();
    ws.push(Write::PutBooking(b));
    ws.push(Write::SetCounter(Counter { next_id: id + 1 }));
    ws.push(Write::Emit(Effect { dest: r.dest, event: Event::Booked, booking: b }));
    Ok((ws, Reply::Created(id)))
}
```

`Cancel` looks up the booking, lets an admin through, requires everyone else
to be the owner and to cancel before `start_at`, and writes `DelBooking` and a
`Cancelled` effect. `apply` gives `Emit` no meaning, since it changes no table.

## The server

The store follows the second tutorial, except for one line: `write` passes
each `Emit` to `h5i_app_pg::outbox::enqueue`, which inserts it into the
`h5i_outbox` table inside the request's transaction. The notification
therefore exists exactly when the booking commits, and a refused or rolled
back request leaves nothing behind.

```rust
k::Write::Emit(e) => outbox::enqueue(tx, t, e.dest, &effect_payload(e)).await?,
```

`principal` turns a verified token into a `Principal` and leaves `now` at 0.
The engine's time replaces it through `stamp`, which converts the engine's
microseconds to the kernel's seconds:

```rust
impl Kernel for BookingApp {
    // ...
    fn stamp(actor: &mut k::Principal, now: Timestamp) {
        actor.now = now.secs();
    }
}
```

The engine reads the time from its `EngineConfig`. The service uses the
database's clock, PostgreSQL's `transaction_timestamp()`, so that every
server of one database agrees on the time, and turns on `monotonic`, which
the section on time below explains:

```rust
pub fn config() -> EngineConfig {
    EngineConfig::default().database_time() // Clock::Database, monotonic: true
}
```

A dispatcher sends what the outbox holds. It needs the registry, which maps
destination ids to endpoints, and an implementation of `Deliver` that sends
one delivery to one endpoint. `Notifier` either logs the notification or POSTs
it as JSON, with the delivery's key in an `Idempotency-Key` header:

```rust
impl Deliver<Endpoint> for Notifier {
    async fn deliver(&self, endpoint: &Endpoint, d: &Delivery) -> Result<(), String> {
        match endpoint {
            Endpoint::Log => { /* tracing::info!(...) */ Ok(()) }
            Endpoint::Http { host, port, path } => /* POST with a timeout */,
        }
    }
}
```

`main` builds the registry from `BOOKING_DESTINATIONS`, for example
`7=log,8=http://10.0.0.5:9000/hooks`, and runs the dispatcher once a second
next to the HTTP server:

```rust
let dispatcher = engine.dispatcher(registry, Notifier::default(), DispatchConfig::default());
tokio::spawn(async move {
    loop {
        dispatcher.run_once().await;   // logs the pass or the error
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
});
```

There are two tests. `tests/postgres.rs` runs random commands, at a clock that
moves forward (`Clock::Manual` for the engine, `MemoryEngine::execute_at` for
the reference), through PostgreSQL and through `MemoryEngine`, and checks that
the replies and the final states agree and that the outbox holds exactly the
notifications of the commands that succeeded, in order. `tests/outbox.rs`
starts a small HTTP receiver and checks that a booking and a cancellation are
posted to the room's endpoint, that a refused command posts nothing, that a
failed post is retried with the same key, and that a destination missing from
the registry is never contacted.

## Writing the specification

Extract the kernel with `h5i app extract examples/app/tutorials/booking` and open
`proofs/Spec.lean`. The state is four lists, as on the board. The new
definitions are about intervals. `Apart` says that two intervals share no
instant, and two bookings are `Compatible` when they are for different rooms
or apart:

```lean
def Apart (s₁ e₁ s₂ e₂ : Nat) : Prop :=
  e₁ ≤ s₂ ∨ e₂ ≤ s₁

def Compatible (b c : Booking) : Prop :=
  b.room = c.room → Apart b.start_at.val b.end_at.val c.start_at.val c.end_at.val
```

The policy now takes the time as well as the user. A booking must be new, by
the caller, of an existing room, in the future, non-empty and compatible with
every existing booking, and a notification must go to the destination the
room registered:

```lean
def allowed (s : St) (u now : Nat) : Write → Prop
  ...
  | .PutBooking b =>
    b.user.val = u ∧ b.id.val = s.next ∧ findRoom s b.room.val ≠ none ∧
      now < b.start_at.val ∧ b.start_at.val < b.end_at.val ∧ ∀ c ∈ s.bookings, Compatible b c
  | .DelBooking id =>
    ∃ b, findBooking s id.val = some b ∧ ((b.user.val = u ∧ now < b.start_at.val) ∨ isAdmin s u)
  | .Emit e =>
    destOf s e.booking.room.val = some e.dest.val
```

"In the future" is part of the policy rather than of the invariant, because it
is a fact about the moment of booking: a booking that was in the future
yesterday may have started today. The invariant lists what stays true, among
it that the bookings are pairwise compatible:

```lean
structure Inv (s : St) : Prop where
  ...
  booked_room : ∀ b ∈ s.bookings, ∃ r ∈ s.rooms, r.id = b.room
  nonempty : ∀ b ∈ s.bookings, b.start_at.val < b.end_at.val
  compatible : s.bookings.Pairwise Compatible
```

`Reachable` is the same as on the board. Since it allows a step by any
principal, it allows any time at every step, including a clock that goes
backwards. Every theorem about reachable states therefore holds whatever the
engine's clock says. A theorem that needs time to move forward uses
`ReachableT` instead, which the section on time below introduces.

## Describing each command

`Commands.lean` specifies the helpers and then each command, as in the second
tutorial. The specification of `free` is an equivalence: `free` returns `true`
exactly when the interval is apart from every booking of the room.

```lean
theorem free_spec (v : alloc.vec.Vec Booking) (room st en : U64) :
    free v room st en ⦃ b => b = true ↔
      ∀ c ∈ v.val, room = c.room → Apart st.val en.val c.start_at.val c.end_at.val ⦄
```

The command lemmas say what a successful `Book` or `Cancel` writes, and two
new lemmas go the other way. `book_ok` says that `Book` succeeds whenever the
policy allows it, and `cancel_ok` says the same of `Cancel`. You will see
shortly why a service like this needs both directions.

## Proving the policy

`writes_of` sums up the command lemmas in five cases, and `authorized` is a
case analysis over them:

```lean
theorem authorized (a : Principal) (s : Snapshot) (c : Command) ws r
    (h : transition a s c = .ok (.Ok (ws, r))) :
    ∀ w ∈ ws.val, allowed (Snapshot.toSt s) a.user.val a.now.val w
```

Unlike the board, it needs no invariant. Its `Emit` case is the first of the
notification properties: every notification of a successful command goes to
the destination registered for its booking's room.

## Proving the invariants

Most of `inv_preserved` works as on the board. The new case is a booking. Its
id is the counter, and every existing id is below the counter, so
`upsert_fresh` shows that storing it appends it to the list. The pairwise
condition on `l ++ [b]` then splits into the condition on `l`, which holds
before, and compatibility of `b` with each booking of `l`, which the kernel
checked. A cancellation keeps a sublist, which keeps every pairwise property.

`Compatible` is stated with inequalities, which is convenient for proofs but
not obviously what "no overlap" means. `apart_iff` connects the two: for
non-empty intervals, being apart means that no instant lies in both. With it,
the invariant becomes a statement about instants:

```lean
theorem no_double_booking {s : St} (h : Reachable s) :
    ∀ b ∈ s.bookings, ∀ c ∈ s.bookings, b.id ≠ c.id → b.room = c.room →
      ∀ t, ¬ (b.start_at.val ≤ t ∧ t < b.end_at.val ∧ c.start_at.val ≤ t ∧ t < c.end_at.val)
```

## Proving the notifications

An effect should describe something that really happened. `effects_sound` says
that a notification goes to the room's destination, that a `Booked`
notification is about a booking that did not exist before the command and
exists after it, and that a `Cancelled` one is about a booking that existed
before and is gone after:

```lean
theorem effects_sound (a : Principal) (s : Snapshot) (c : Command) ws r
    (hinv : Inv (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r))) :
    let st := Snapshot.toSt s
    ∀ e, .Emit e ∈ ws.val →
      destOf st e.booking.room.val = some e.dest.val ∧
      (e.event = .Booked → e.booking ∉ st.bookings ∧ e.booking ∈ (applyAll st ws.val).bookings) ∧
      (e.event = .Cancelled → e.booking ∈ st.bookings ∧ e.booking ∉ (applyAll st ws.val).bookings)
```

The hypothesis `Inv` holds in every reachable state by `reachable_inv`. Since
effects are part of the write set, "only when committed" needs no separate
proof in the kernel: a refused command has no write set, and the store writes
the effects in the transaction that commits the rows. `changes_announced`
proves the converse, that every booking and every cancellation comes with its
notification.

The kernel's part ends at the destination id. The rest of the way, from the
outbox row to the endpoint, is the trusted dispatcher. PostgreSQL integration
tests cover retries, stable delivery keys, unknown destinations and concurrent
dispatchers. Receivers must deduplicate because a crash after sending and
before recording success can send the same row again.

## Accepted commands and scenarios

A service that refuses every command satisfies all of the theorems so far, so
`book_accepted` and `cancel_accepted` state the other direction: a booking
that the policy allows is accepted, and so is a cancellation by an admin, or
by the owner before the start. The only extra hypothesis of `book_accepted`
is that the counter has not reached `2^64 - 1`.

`Scenarios.lean` runs the extracted code on concrete states. With room 0 and a
booking `[1, 2)`, `adjacent_accepted` shows that `[2, 3)` is accepted with the
expected writes and notification, `overlap_refused` shows that `[1, 3)` gets
`Taken`, and further scenarios cover a booking in the past, a stranger's and a
late cancellation, and the owner's and the admin's cancellations.
The `h5i_eval` tactic uses the registered loop specs to make each of these
concrete command checks a one-line proof.
`s1_reachable` shows that the state with the first booking is reachable, by
running `AddAdmin`, `CreateRoom` and `Book` from the empty service, so the
theorems about reachable states apply to it.

## Committing a write set

`schema!` generates `apply`, the table operations and their loop lemmas.
`Apply.lean` proves that its write dispatch computes `Spec.applyAll`; `Emit`
does not change the tables. `Storage.lean` instantiates the shared store
theorem, proving that the planned SQL writes leave exactly those rows and that
loading them decodes the resulting state up to row order.

## Time that never goes back

The rule "owners cancel before the start" is meant to say more than what one
command checks: once a booking has started, its owner can no longer cancel
it. That reading needs time to move forward. If one request commits at 10:05,
after the booking started at 10:00, and a later request carries 9:55, the
owner cancels a meeting that the service already treated as under way.

With several servers, or one whose clock is corrected backwards, this
happens. PostgreSQL orders transactions, not clocks, so a request that
commits later may carry an earlier time. The engine's `monotonic` option
rules it out. The engine keeps the time of each tenant's latest commit in the
table `h5i_clock` and uses the later of that and the clock, reading and
advancing the row in the request's transaction. The trusted engine contract
says that committed times then never decrease in commit order within a tenant;
the PostgreSQL tests exercise this implementation.

In the proofs, `H5iAppLib` provides `ReachableT`, reachability paired with the
time of the latest commit, whose step requires the principal's time to be no
earlier, and `StepsT`, the same from a given state for callers that pass a
filter. `Spec.lean` instantiates them for this service:

```lean
abbrev ReachableT : St → Nat → Prop :=
  H5iAppLib.ReachableT transition (·.now.val) Snapshot.toSt (fun st ws => applyAll st ws.val) init

abbrev StepsT (allow : Principal → St → Prop) : St → Nat → St → Nat → Prop :=
  H5iAppLib.StepsT transition (·.now.val) Snapshot.toSt (fun st ws => applyAll st ws.val) allow
```

`Clock.lean` proves that once a commit has happened at or after a booking's
start, only an admin can remove it:

```lean
theorem started_stays {st st' : St} {t t' : Nat} (hr : ReachableT st t)
    (h : StepsT (fun a st => ¬ isAdmin st a.user.val) st t st' t') {b : Booking}
    (hb : b ∈ st.bookings) (hstart : b.start_at.val ≤ t) : b ∈ st'.bookings
```

The proof follows `H5iAppLib.StepsT.preserve`: every step by a non-admin keeps
the booking, since `Book` uses a fresh id and `Cancel` of this booking needs
an admin or a time before its start, which is at most `t` and so before the
step's time. The invariants come from `Reachable`, because every timed run is
a run (`reachableT_reachable`).

The theorem is false without monotonic time, and `clock_back_cancels` shows
it on the extracted code. The admin sets up room 0 and user 2 books `[1, 2)`
at time 0, user 3 lists the bookings at time 5, and then user 2, who is not an
admin, cancels the booking with a clock that reads 0. Every step is accepted,
and only the last one goes back in time, which is the one `StepsT` excludes.
`tests/postgres.rs` runs the same sequence on PostgreSQL: without `monotonic`
the cancellation succeeds, and with it the engine uses the later time and the
kernel answers `started`.

## Introducing a bug

The classic mistake with intervals is to treat them as closed, so that two
bookings that touch count as overlapping. Change the comparisons in `free`:

```rust
if v[i].room == room && start_at <= v[i].end_at && v[i].start_at <= end_at {
```

After re-extracting, `lake build` fails in the proof of `free_spec`:

```
error: Commands.lean:69:2: unsolved goals
case h1
v : alloc.vec.Vec Booking
room st en : U64
j : Usize
hj : ↑j ≤ (↑v).length
h✝³ : j < v.len
b : Booking
_ : [> let b ← v.index_usize j <]
b_post : b = (↑v)[↑j]
h✝² : (↑v)[↑j].room = room
h✝¹ : ↑st ≤ ↑(↑v)[↑j].end_at
h✝ : ↑(↑v)[↑j].start_at ≤ ↑en
⊢ ∃ (h : ↑j < (↑v).length), ↑st < ↑(↑v)[↑j].end_at ∧ ↑(↑v)[↑j].start_at < ↑en
```

This is the step where `free` finds a clash and returns `false`. The code
stopped because the new interval starts no later than the existing one ends,
and Lean asks to show that it starts strictly earlier, which is not true when
the two only touch.

Notice which theorem caught the bug. With closed intervals the kernel refuses
more, never less, so `authorized` and `no_double_booking` remain true: a
service that refuses adjacent bookings is still safe, only wrong. What fails
is the direction of `free_spec` that says `free` accepts every compatible
interval, which `book_accepted` and the scenario `adjacent_accepted` rely on.
Had `free_spec` been stated as an implication, the bug would have passed.

## Limits of the proofs

The theorems hold for every value of `now`, so a wrong clock cannot create
overlapping bookings or send a notification to the wrong place. It can make
the service accept a booking that has already started or let an owner cancel
one that is under way, since "future" means future according to the engine.
The clock is therefore part of the trusted base, like the JSON decoder. The
service reads the database's clock, so its servers agree with each other,
but nothing checks that the database's clock is right. With `monotonic`, a
clock that is behind cannot take the service back in time, but a clock that
is ahead moves every later request forward until the real time catches up.
`started_stays` covers only runs of the engine with `monotonic` on.

Notifications are delivered at least once. A dispatcher that crashes after a
POST but before recording it sends it again with the same key, and the
receiver has to drop the repeat. A dispatcher sends each batch in commit
order, but a delivery that fails is retried later, so a cancellation may still
arrive before the booking it cancels, and a receiver should order by booking
and event rather than by arrival. A notification to a
destination missing from the registry, or one that failed on every attempt,
is marked dead and never sent. The kernel theorems stop at the destination id.
The dispatcher's properties are proven about a model of its protocol rather
than its code, and the registry, `Notifier` and the enqueue in `write` are
trusted, although the tests check them.

## Exercises

1. Let admins change a room's destination with a `SetDest` command. Which
   theorems need a new case, and where do cancellations of existing bookings
   now get sent?
2. Limit a booking to eight hours. Add the rule to `allowed`, then find the
   theorem that fails before you change the kernel.
3. State and prove that a user who is not an admin can never cancel a booking
   that has started, directly in terms of `transition`.
4. Add a scenario in which the owner cancels `[1, 2)` and another user then
   books the same interval, and prove it on the extracted code.
