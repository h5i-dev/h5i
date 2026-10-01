import Apply
/-!
# Scenarios

Concrete runs of the extracted code: the theorems' hypotheses can be met,
adjacent bookings are both accepted, an overlapping one is refused, and only
the owner or an admin may cancel.

Room 0 notifies destination 7; user 1 is the admin, users 2 and 3 book.
-/
open Aeneas Aeneas.Std Result booking_kernel booking_kernel.Spec booking_kernel.Commands
  booking_kernel.Theorems H5iAppLib

namespace booking_kernel.Scenarios

/-- User `u` at time `t`. -/
def at_ (u t : U64) : Principal := ⟨1#u64, u, t⟩

def bk1 : Booking := ⟨1#u64, 0#u64, 2#u64, 1#u64, 2#u64⟩
def bk2 : Booking := ⟨2#u64, 0#u64, 3#u64, 2#u64, 3#u64⟩

/-- One room and no bookings yet. -/
def s0 : Snapshot := ⟨⟨1#u64⟩, vecOf [⟨1#u64⟩], vecOf [⟨0#u64, 7#u64⟩], vecOf []⟩
/-- After user 2 booked `[1, 2)`. -/
def s1 : Snapshot := ⟨⟨2#u64⟩, vecOf [⟨1#u64⟩], vecOf [⟨0#u64, 7#u64⟩], vecOf [bk1]⟩

theorem book_first :
    transition (at_ 2#u64 0#u64) s0 (.Book 0#u64 1#u64 2#u64) ⦃ o => ∃ ws, o = .Ok (ws, .Created 1#u64) ∧
      ws.val = [.PutBooking bk1, .SetCounter ⟨2#u64⟩, .Emit ⟨7#u64, .Booked, bk1⟩] ⦄ := by
  h5i_eval (transition book) [s0, bk1, at_, vecOf, core.num.U64.MAX, U64.rMax]

/-- `[2, 3)` starts where `[1, 2)` ends, so it is accepted. -/
theorem adjacent_accepted :
    transition (at_ 3#u64 0#u64) s1 (.Book 0#u64 2#u64 3#u64) ⦃ o => ∃ ws, o = .Ok (ws, .Created 2#u64) ∧
      ws.val = [.PutBooking bk2, .SetCounter ⟨3#u64⟩, .Emit ⟨7#u64, .Booked, bk2⟩] ⦄ := by
  h5i_eval (transition book) [s1, bk1, bk2, at_, vecOf, Apart, core.num.U64.MAX, U64.rMax]

/-- `[1, 3)` shares `[1, 2)` with the first booking. -/
theorem overlap_refused :
    transition (at_ 3#u64 0#u64) s1 (.Book 0#u64 1#u64 3#u64) ⦃ o => o = .Err .Taken ⦄ := by
  h5i_eval (transition book) [s1, bk1, at_, vecOf, Apart]

theorem past_refused :
    transition (at_ 3#u64 5#u64) s1 (.Book 0#u64 5#u64 6#u64) ⦃ o => o = .Err .InThePast ⦄ := by
  h5i_eval (transition book) [s1, bk1, at_, vecOf]

/-- Another user may not cancel user 2's booking. -/
theorem stranger_refused :
    transition (at_ 3#u64 0#u64) s1 (.Cancel 1#u64) ⦃ o => o = .Err .Forbidden ⦄ := by
  h5i_eval (transition cancel) [s1, bk1, at_, vecOf]

/-- The owner may not cancel once the booking has started. -/
theorem started_refused :
    transition (at_ 2#u64 1#u64) s1 (.Cancel 1#u64) ⦃ o => o = .Err .Started ⦄ := by
  h5i_eval (transition cancel) [s1, bk1, at_, vecOf]

/-- The owner cancels before the start, and room 0's destination hears of it. -/
theorem owner_cancels :
    transition (at_ 2#u64 0#u64) s1 (.Cancel 1#u64) ⦃ o => ∃ ws, o = .Ok (ws, .Done) ∧
      ws.val = [.DelBooking 1#u64, .Emit ⟨7#u64, .Cancelled, bk1⟩] ⦄ := by
  h5i_eval (transition cancel) [s1, bk1, at_, vecOf]

/-- The admin cancels after the start. -/
theorem admin_cancels :
    transition (at_ 1#u64 9#u64) s1 (.Cancel 1#u64) ⦃ o => ∃ ws, o = .Ok (ws, .Done) ∧
      ws.val = [.DelBooking 1#u64, .Emit ⟨7#u64, .Cancelled, bk1⟩] ⦄ := by
  h5i_eval (transition cancel) [s1, bk1, at_, vecOf]

/-! ## The scenario states are reachable -/

def sEmpty : Snapshot := ⟨⟨0#u64⟩, vecOf [], vecOf [], vecOf []⟩
/-- User 1 is the admin. -/
def sAdmin : Snapshot := ⟨⟨0#u64⟩, vecOf [⟨1#u64⟩], vecOf [], vecOf []⟩

theorem add_first_admin :
    transition (at_ 1#u64 0#u64) sEmpty (.AddAdmin 1#u64) ⦃ o => ∃ ws, o = .Ok (ws, .Done) ∧
      ws.val = [.PutAdmin ⟨1#u64⟩] ⦄ := by
  unfold transition add_admin
  dsimp only
  split <;> step*
  all_goals simp_all [sEmpty, at_, vecOf]

theorem create_first_room :
    transition (at_ 1#u64 0#u64) sAdmin (.CreateRoom 7#u64) ⦃ o => ∃ ws, o = .Ok (ws, .Created 0#u64) ∧
      ws.val = [.PutRoom ⟨0#u64, 7#u64⟩, .SetCounter ⟨1#u64⟩] ⦄ := by
  unfold transition create_room
  step*
  all_goals simp_all [sAdmin, at_, vecOf, core.num.U64.MAX, U64.rMax]
  all_goals scalar_tac

/-- Run a step of `Reachable` from a spec of the form above. -/
theorem reach {a s c} {st' : St} {P : alloc.vec.Vec Write → Prop} {rep : Reply}
    (hs : Reachable (Snapshot.toSt s))
    (h : transition a s c ⦃ o => ∃ ws, o = .Ok (ws, rep) ∧ P ws ⦄)
    (happ : ∀ ws, P ws → applyAll (Snapshot.toSt s) ws.val = st') : Reachable st' := by
  obtain ⟨o, ho, ws, rfl, hp⟩ := (WP.spec_equiv_exists _ _).1 h
  exact happ ws hp ▸ Reachable.step hs ho

/-- Adding an admin, creating a room and booking it leads to `s1`, so the
theorems about reachable states apply to it. -/
theorem s1_reachable : Reachable (Snapshot.toSt s1) := by
  have h0 : Reachable (Snapshot.toSt sEmpty) := by
    simp only [Snapshot.toSt, sEmpty, vecOf]; exact .init
  have h1 : Reachable (Snapshot.toSt sAdmin) := reach h0 add_first_admin (fun ws h => by
    simp [h, applyAll, applyWrite, upsert, Snapshot.toSt, sEmpty, sAdmin, vecOf])
  have h2 : Reachable (Snapshot.toSt s0) := reach h1 create_first_room (fun ws h => by
    simp [h, applyAll, applyWrite, upsert, Snapshot.toSt, sAdmin, s0, vecOf])
  exact reach h2 book_first (fun ws h => by
    simp [h, applyAll, applyWrite, upsert, Snapshot.toSt, s0, s1, vecOf])

end booking_kernel.Scenarios
