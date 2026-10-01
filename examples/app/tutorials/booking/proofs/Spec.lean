import BookingKernel
import H5iAppLib
/-!
# What the booking service should do

The file to review: the policy (including where notifications go), what a
write does, and the invariants, on plain lists. Times are Unix seconds.
-/
open Aeneas Aeneas.Std booking_kernel

namespace booking_kernel.Spec

/-- The state as lists: the next id, the admins, the rooms and the bookings. -/
structure St where
  next : Nat
  admins : List Admin
  rooms : List Room
  bookings : List Booking

def Snapshot.toSt (s : Snapshot) : St :=
  ⟨s.counter.next_id.val, s.admins.val, s.rooms.val, s.bookings.val⟩

def isAdmin (s : St) (u : Nat) : Prop :=
  ∃ m ∈ s.admins, m.user.val = u

def findRoom (s : St) (id : Nat) : Option Room :=
  s.rooms.find? (fun r => r.id.val = id)

/-- The destination registered for a room, if the room exists. -/
def destOf (s : St) (room : Nat) : Option Nat :=
  (findRoom s room).map (·.dest.val)

def findBooking (s : St) (id : Nat) : Option Booking :=
  s.bookings.find? (fun b => b.id.val = id)

/-- `[s₁, e₁)` and `[s₂, e₂)` share no instant: one ends before the other starts. -/
def Apart (s₁ e₁ s₂ e₂ : Nat) : Prop :=
  e₁ ≤ s₂ ∨ e₂ ≤ s₁

/-- Two bookings may coexist if they are for different rooms or their intervals are apart. -/
def Compatible (b c : Booking) : Prop :=
  b.room = c.room → Apart b.start_at.val b.end_at.val c.start_at.val c.end_at.val

/-- The policy: may user `u` make write `w` in state `s` at time `now`? -/
def allowed (s : St) (u now : Nat) : Write → Prop
  | .PutAdmin m =>
    -- As in tutorial 2: an admin adds anyone; with no admins, a user adds themselves.
    isAdmin s u ∨ (s.admins = [] ∧ m.user.val = u)
  | .PutRoom r => isAdmin s u ∧ r.id.val = s.next
  | .PutBooking b =>
    -- A new booking by `u` of an existing room, in the future, compatible with every other.
    b.user.val = u ∧ b.id.val = s.next ∧ findRoom s b.room.val ≠ none ∧
      now < b.start_at.val ∧ b.start_at.val < b.end_at.val ∧ ∀ c ∈ s.bookings, Compatible b c
  | .DelBooking id =>
    -- The owner before the start, or an admin at any time.
    ∃ b, findBooking s id.val = some b ∧ ((b.user.val = u ∧ now < b.start_at.val) ∨ isAdmin s u)
  | .SetCounter c => c.next_id.val = s.next + 1
  | .Emit e =>
    -- A notification goes to the destination registered for the booking's room.
    destOf s e.booking.room.val = some e.dest.val

/-- What a write does to the state. Effects change no table. -/
def applyWrite (s : St) : Write → St
  | .PutAdmin m => { s with admins := H5iAppLib.upsert (·.user) m s.admins }
  | .PutRoom r => { s with rooms := H5iAppLib.upsert (·.id) r s.rooms }
  | .PutBooking b => { s with bookings := H5iAppLib.upsert (·.id) b s.bookings }
  | .DelBooking id => { s with bookings := s.bookings.filter (fun b => b.id ≠ id) }
  | .SetCounter c => { s with next := c.next_id.val }
  | .Emit _ => s

def applyAll (s : St) (ws : List Write) : St := ws.foldl applyWrite s

/-- Facts that hold in every state the service can reach. -/
structure Inv (s : St) : Prop where
  room_keys : (s.rooms.map (·.id)).Nodup
  booking_keys : (s.bookings.map (·.id)).Nodup
  rooms_fresh : ∀ r ∈ s.rooms, r.id.val < s.next
  bookings_fresh : ∀ b ∈ s.bookings, b.id.val < s.next
  /-- Every booking is of an existing room. -/
  booked_room : ∀ b ∈ s.bookings, ∃ r ∈ s.rooms, r.id = b.room
  /-- Every booking has a non-empty interval. -/
  nonempty : ∀ b ∈ s.bookings, b.start_at.val < b.end_at.val
  /-- No two bookings of a room overlap. -/
  compatible : s.bookings.Pairwise Compatible

def init : St := ⟨0, [], [], []⟩

/-- States reachable from an empty service, at any times the shell supplies. -/
inductive Reachable : St → Prop
  | init : Reachable init
  | step {a s c ws r} : Reachable (Snapshot.toSt s) → transition a s c = .ok (.Ok (ws, r)) →
      Reachable (applyAll (Snapshot.toSt s) ws.val)

/-- Reachable states, with the time of the latest commit, when the engine's
clock never goes back (`EngineConfig::monotonic`). -/
abbrev ReachableT : St → Nat → Prop :=
  H5iAppLib.ReachableT transition (·.now.val) Snapshot.toSt (fun st ws => applyAll st ws.val) init

/-- Runs from a state at a time, of commands by callers that pass `allow`,
with time never going back. -/
abbrev StepsT (allow : Principal → St → Prop) : St → Nat → St → Nat → Prop :=
  H5iAppLib.StepsT transition (·.now.val) Snapshot.toSt (fun st ws => applyAll st ws.val) allow

end booking_kernel.Spec
