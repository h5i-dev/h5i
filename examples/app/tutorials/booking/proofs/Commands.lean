import Spec
/-!
# What each command does

List specs for the helpers, then one lemma per command: what it writes when
it succeeds. `book_ok` and `cancel_ok` say when a command succeeds.
`Theorems.lean` uses only these lemmas.
-/
open Aeneas Aeneas.Std Result booking_kernel booking_kernel.Spec H5iAppLib

namespace booking_kernel.Commands

attribute [simp] u64_val_eq

/-! ## Helpers -/

@[step] theorem one_spec (w : Write) : one w ⦃ v => v.val = [w] ⦄ := by
  unfold one; step*

@[step] theorem is_admin_spec (v : alloc.vec.Vec Admin) (u : U64) :
    is_admin v u ⦃ b => b = true ↔ ∃ m ∈ v.val, m.user.val = u.val ⦄ := by
  unfold is_admin is_admin_loop
  apply WP.spec_mono (loop_search v.val (fun m => decide (m.user = u)) (fun b : Bool => b)
    (fun _ _ => true) false _ ?_ 0#usize (by simp))
  · intro r hr
    rw [hr, searchFrom_const]
    simp
  · intro j hj; unfold is_admin_loop.body; h5i_step

@[step] theorem find_room_spec (v : alloc.vec.Vec Room) (id : U64) :
    find_room v id ⦃ o => o = v.val.find? (fun r => r.id.val = id.val) ⦄ := by
  unfold find_room find_room_loop
  apply WP.spec_mono (loop_search v.val (fun r => decide (r.id = id)) (fun o : Option Room => o)
    (fun _ r => some r) none _ ?_ 0#usize (by simp))
  · intro r hr; rw [search_find _ _ _ hr]; simp
  · intro j hj; unfold find_room_loop.body; h5i_step

@[step] theorem find_booking_spec (v : alloc.vec.Vec Booking) (id : U64) :
    find_booking v id ⦃ o => o = v.val.find? (fun b => b.id.val = id.val) ⦄ := by
  unfold find_booking find_booking_loop
  apply WP.spec_mono (loop_search v.val (fun b => decide (b.id = id)) (fun o : Option Booking => o)
    (fun _ b => some b) none _ ?_ 0#usize (by simp))
  · intro r hr; rw [search_find _ _ _ hr]; simp
  · intro j hj; unfold find_booking_loop.body; h5i_step

/-- `free` holds exactly when `[st, en)` is apart from every booking of the room. -/
@[step] theorem free_spec (v : alloc.vec.Vec Booking) (room st en : U64) :
    free v room st en ⦃ b => b = true ↔
      ∀ c ∈ v.val, room = c.room → Apart st.val en.val c.start_at.val c.end_at.val ⦄ := by
  unfold free free_loop
  apply WP.spec_mono (loop_search v.val
    (fun c => decide (c.room = room ∧ st.val < c.end_at.val ∧ c.start_at.val < en.val))
    (fun b : Bool => b) (fun _ _ => false) true _ ?_ 0#usize (by simp))
  · intro r hr
    rw [hr, searchFrom_const]
    simp only [List.any_eq_true, decide_eq_true_eq, Apart]
    split
    · rename_i h
      obtain ⟨c, hc, h1, h2, h3⟩ := h
      simp only [Bool.false_eq_true, false_iff, not_forall]
      exact ⟨c, hc, h1.symm, by omega⟩
    · rename_i h
      simp only [not_exists, not_and] at h
      simp only [true_iff]
      intro c hc hr
      have := h c hc hr.symm
      omega
  · intro j hj; unfold free_loop.body; h5i_step

theorem findRoom_toSt (s : Snapshot) (id : U64) :
    s.rooms.val.find? (fun r => r.id.val = id.val) = findRoom (Snapshot.toSt s) id.val := rfl

theorem findBooking_toSt (s : Snapshot) (id : U64) :
    s.bookings.val.find? (fun b => b.id.val = id.val) = findBooking (Snapshot.toSt s) id.val := rfl

/-! ## Commands

What a successful run writes, and which facts made it succeed. -/

theorem add_admin_spec (u : U64) (s : Snapshot) (target : U64) :
    add_admin u s target ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      (isAdmin (Snapshot.toSt s) u.val ∨ ((Snapshot.toSt s).admins = [] ∧ target = u)) ∧
        ws.val = [.PutAdmin ⟨target⟩] ⦄ := by
  unfold add_admin
  dsimp only
  split <;> step*
  all_goals
    intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h
  all_goals try exact ⟨.inl (by simpa [isAdmin, Snapshot.toSt] using b_post.1 ‹b = true›), v_post⟩
  refine ⟨.inr ⟨?_, by simpa using ‹decide (target = u) = true›⟩, v_post⟩
  have := ‹s.admins.len = 0#usize›
  simp only [Snapshot.toSt]
  exact List.eq_nil_of_length_eq_zero (by scalar_tac)

theorem create_room_spec (u : U64) (s : Snapshot) (dest : U64) :
    create_room u s dest ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → isAdmin (Snapshot.toSt s) u.val ∧
      ∃ c : Counter, c.next_id.val = s.counter.next_id.val + 1 ∧
        ws.val = [.PutRoom ⟨s.counter.next_id, dest⟩, .SetCounter c] ⦄ := by
  unfold create_room
  step*
  -- Left: `next_id + 1` cannot overflow, and the result.
  all_goals try (simp only [core.num.U64.MAX, U64.rMax] at *; scalar_tac)
  intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h
  refine ⟨by simpa [isAdmin, Snapshot.toSt] using b_post.1 ‹b = true›, _, i_post, ?_⟩
  simp [ws1_post, ws_post]

/-- The booking a successful `Book` creates. -/
def newBooking (a : Principal) (s : Snapshot) (room st en : U64) : Booking :=
  ⟨s.counter.next_id, room, a.user, st, en⟩

theorem book_spec (a : Principal) (s : Snapshot) (room st en : U64) :
    book a s room st en ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      let b := newBooking a s room st en
      ∃ rm, findRoom (Snapshot.toSt s) room.val = some rm ∧
        a.now.val < st.val ∧ st.val < en.val ∧ (∀ c ∈ (Snapshot.toSt s).bookings, Compatible b c) ∧
        ∃ c : Counter, c.next_id.val = s.counter.next_id.val + 1 ∧
          ws.val = [.PutBooking b, .SetCounter c, .Emit ⟨rm.dest, .Booked, b⟩] ⦄ := by
  unfold book
  step*
  all_goals try (simp only [core.num.U64.MAX, U64.rMax] at *; scalar_tac)
  intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h
  refine ⟨r, by rw [← findRoom_toSt, ← o_post]; assumption, by scalar_tac, by scalar_tac,
    fun c hc hr => b_post.1 ‹_› c hc hr, _, i_post, ?_⟩
  simp [ws2_post, ws1_post, ws_post, newBooking]

/-- `Book` succeeds whenever the room exists, the interval is non-empty, in
the future and compatible with every booking, and ids are not exhausted. -/
theorem book_ok (a : Principal) (s : Snapshot) (room st en : U64)
    (hroom : findRoom (Snapshot.toSt s) room.val ≠ none) (hnow : a.now.val < st.val) (hlt : st.val < en.val)
    (hfree : ∀ c ∈ (Snapshot.toSt s).bookings, Compatible (newBooking a s room st en) c)
    (hid : s.counter.next_id.val < U64.max) :
    book a s room st en ⦃ r => ∃ ws, r = .Ok (ws, .Created s.counter.next_id) ⦄ := by
  unfold book
  step*
  -- Each refusal contradicts a hypothesis.
  all_goals exfalso
  all_goals first
    | exact hroom (by rw [← findRoom_toSt, ← o_post]; assumption)
    | exact ‹¬b = true› (b_post.2 (fun c hc hr => hfree c hc hr))
    | (simp only [core.num.U64.MAX, U64.rMax] at *; scalar_tac)

theorem cancel_spec (a : Principal) (s : Snapshot) (id : U64) :
    cancel a s id ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      ∃ b rm, findBooking (Snapshot.toSt s) id.val = some b ∧
        (isAdmin (Snapshot.toSt s) a.user.val ∨ (b.user = a.user ∧ a.now.val < b.start_at.val)) ∧
        findRoom (Snapshot.toSt s) b.room.val = some rm ∧
        ws.val = [.DelBooking id, .Emit ⟨rm.dest, .Cancelled, b⟩] ⦄ := by
  unfold cancel
  step*
  all_goals
    intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h
  all_goals have hb : findBooking (Snapshot.toSt s) id.val = some b := by rw [← findBooking_toSt, ← o_post]; assumption
  all_goals have hr : findRoom (Snapshot.toSt s) b.room.val = some r := by rw [← findRoom_toSt, ← o1_post]; assumption
  · exact ⟨b, r, hb, .inl (by simpa [isAdmin, Snapshot.toSt] using b1_post.1 ‹_›), hr, by simp [ws1_post, ws_post]⟩
  · refine ⟨b, r, hb, .inr ⟨by simpa using ‹¬(b.user != a.user) = true›, by scalar_tac⟩, hr, by simp [ws1_post, ws_post]⟩

/-- `Cancel` succeeds for an admin, or for the owner before the start, as
long as the booking's room exists. -/
theorem cancel_ok (a : Principal) (s : Snapshot) (id : U64) (bk : Booking)
    (hb : findBooking (Snapshot.toSt s) id.val = some bk)
    (hwho : isAdmin (Snapshot.toSt s) a.user.val ∨ (bk.user = a.user ∧ a.now.val < bk.start_at.val))
    (hroom : findRoom (Snapshot.toSt s) bk.room.val ≠ none) :
    cancel a s id ⦃ r => ∃ ws, r = .Ok (ws, .Done) ⦄ := by
  rw [← findBooking_toSt] at hb
  rw [← findRoom_toSt] at hroom
  unfold cancel
  step*
  all_goals exfalso
  -- The booking `step*` found is `bk`.
  all_goals try
    have hbk : b = bk := by
      have h1 := ‹o = some b›; rw [o_post, hb] at h1; exact (Option.some.inj h1).symm
    subst hbk
  all_goals first
    | exact hroom (by rw [← ‹o1 = none›, o1_post])
    | (rcases hwho with hadm | ⟨hu, hnow⟩
       · exact ‹¬b1 = true› (b1_post.2 (by simpa [isAdmin, Snapshot.toSt] using hadm))
       · scalar_tac)

end booking_kernel.Commands
