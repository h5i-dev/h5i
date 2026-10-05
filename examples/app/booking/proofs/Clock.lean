import Scenarios
/-!
# Time that never goes back

"Owners cancel before the start" should mean a started booking stays. The
kernel compares the start with the cancel request's time, so this needs a
clock that never goes back (`EngineConfig::monotonic`, modeled by
`ReachableT` and `StepsT`).

- `started_stays`: once a commit has happened at or after a booking's start,
  no run of commands by non-admins removes it.
- `clock_back_cancels`: without monotonic time this fails. After a commit at
  time 5, the owner of a booking that started at 1 cancels it at time 0.
-/
open Aeneas Aeneas.Std Result booking_kernel booking_kernel.Spec booking_kernel.Commands
  booking_kernel.Theorems booking_kernel.Scenarios H5iAppLib

namespace booking_kernel.Clock

/-- Timed runs are runs, so every invariant holds on them. -/
theorem reachableT_reachable {st : St} {t : Nat} (h : ReachableT st t) : Reachable st :=
  H5iAppLib.ReachableT.forget Reachable .init (fun _ _ _ _ _ hs ht => .step hs ht) h

/-- A command by a non-admin at time `t` or later keeps a booking that started
by `t`. -/
theorem started_step (a : Principal) (s : Snapshot) (c : Command) ws r {t : Nat} {b : Booking}
    (hinv : Inv (Snapshot.toSt s)) (hb : b ∈ (Snapshot.toSt s).bookings) (hstart : b.start_at.val ≤ t)
    (hle : t ≤ a.now.val) (hna : ¬ isAdmin (Snapshot.toSt s) a.user.val)
    (h : transition a s c = .ok (.Ok (ws, r))) : b ∈ (applyAll (Snapshot.toSt s) ws.val).bookings := by
  rcases writes_of a s c ws r h with
    hws | ⟨tg, ha, hws⟩ | ⟨d, cnt, ha, hc, hws⟩ | ⟨b', rm, cnt, hid, hu, hrm, hnow, hlt, hfree, hc, hws⟩ |
    ⟨id, b', rm, hb', hwho, hrm, hws⟩ <;> rw [hws] <;>
    simp only [applyAll, List.foldl_cons, List.foldl_nil, applyWrite]
  · exact hb
  · exact hb
  · exact hb
  · -- Book: the new booking takes the next id, so no existing booking is replaced.
    refine mem_upsert_of_ne hb fun he => ?_
    have := hinv.bookings_fresh b hb
    rw [show b.id = b'.id from he, hid] at this
    exact Nat.lt_irrefl _ this
  · -- Cancel: removing `b` takes an admin, or its owner before the start.
    refine List.mem_filter.2 ⟨hb, ?_⟩
    simp only [decide_eq_true_eq, ne_eq]
    intro he
    obtain ⟨hm, hid⟩ := findBooking_mem hb'
    have : b' = b := List.inj_on_of_nodup_map hinv.booking_keys hm hb
      ((u64_val_eq _ _).1 (by rw [hid, he]))
    subst this
    rcases hwho with ha | ⟨-, hnow⟩
    · exact hna ha
    · omega

/-- Once the engine's clock has passed a booking's start, only an admin can
remove it: every later run of commands by non-admins keeps it. -/
theorem started_stays {st st' : St} {t t' : Nat} (hr : ReachableT st t)
    (h : StepsT (fun a st => ¬ isAdmin st a.user.val) st t st' t') {b : Booking}
    (hb : b ∈ st.bookings) (hstart : b.start_at.val ≤ t) : b ∈ st'.bookings :=
  (StepsT.preserve (fun st t => b ∈ st.bookings ∧ b.start_at.val ≤ t)
    (fun a s c ws r _ hr' hq hle hna ht =>
      ⟨started_step a s c ws r (reachable_inv (reachableT_reachable hr')) hq.1 hq.2 hle hna ht,
        Nat.le_trans hq.2 hle⟩)
    hr ⟨hb, hstart⟩ h).1

/-! ## Without monotonic time -/

/-- Run a step of `ReachableT` from a spec, like `reach`. -/
theorem reachT {a s c t} {st' : St} {P : alloc.vec.Vec Write → Prop} {rep : Reply}
    (hs : ReachableT (Snapshot.toSt s) t) (hle : t ≤ a.now.val)
    (h : transition a s c ⦃ o => ∃ ws, o = .Ok (ws, rep) ∧ P ws ⦄)
    (happ : ∀ ws, P ws → applyAll (Snapshot.toSt s) ws.val = st') : ReachableT st' a.now.val := by
  obtain ⟨o, ho, ws, rfl, hp⟩ := (WP.spec_equiv_exists _ _).1 h
  exact happ ws hp ▸ H5iAppLib.ReachableT.step hs hle ho

theorem list_s1 (u t : U64) :
    transition (at_ u t) s1 .List ⦃ o => ∃ ws, o = .Ok (ws, .Bookings s1.bookings) ∧ ws.val = [] ⦄ := by
  simp [transition, vec_clone_eq Booking.Insts.CoreCloneClone s1.bookings (fun _ => rfl)]

/-- The admin adds itself, creates room 0 and user 2 books `[1, 2)`, all at
time 0; then user 3 lists the bookings at time 5. -/
theorem s1_at_5 : ReachableT (Snapshot.toSt s1) 5 := by
  have h0 : ReachableT (Snapshot.toSt sEmpty) 0 := by
    simp only [Snapshot.toSt, sEmpty, vecOf]; exact .init
  have h1 : ReachableT (Snapshot.toSt sAdmin) 0 := reachT h0 (Nat.le_refl _) add_first_admin (fun ws h => by
    simp [h, applyAll, applyWrite, upsert, Snapshot.toSt, sEmpty, sAdmin, vecOf])
  have h2 : ReachableT (Snapshot.toSt s0) 0 := reachT h1 (Nat.le_refl _) create_first_room (fun ws h => by
    simp [h, applyAll, applyWrite, upsert, Snapshot.toSt, sAdmin, s0, vecOf])
  have h3 : ReachableT (Snapshot.toSt s1) 0 := reachT h2 (Nat.le_refl _) book_first (fun ws h => by
    simp [h, applyAll, applyWrite, upsert, Snapshot.toSt, s0, s1, vecOf])
  exact reachT (a := at_ 3#u64 5#u64) h3 (Nat.zero_le _) (list_s1 _ _) (fun ws h => by simp [h, applyAll])

/-- `started_stays` needs monotonic time: after the commit at time 5, booking
1 (`[1, 2)`) has started, yet its owner (not an admin) cancels it at time 0.
`StepsT` excludes this step. -/
theorem clock_back_cancels :
    ReachableT (Snapshot.toSt s1) 5 ∧ bk1 ∈ (Snapshot.toSt s1).bookings ∧ bk1.start_at.val ≤ 5 ∧
    ¬ isAdmin (Snapshot.toSt s1) 2 ∧
    ∃ ws, transition (at_ 2#u64 0#u64) s1 (.Cancel 1#u64) = ok (.Ok (ws, .Done)) ∧
      bk1 ∉ (applyAll (Snapshot.toSt s1) ws.val).bookings := by
  refine ⟨s1_at_5, by simp [Snapshot.toSt, s1, vecOf], by simp [bk1], by simp [isAdmin, Snapshot.toSt, s1, vecOf], ?_⟩
  obtain ⟨o, ho, ws, rfl, hws⟩ := (WP.spec_equiv_exists _ _).1 owner_cancels
  exact ⟨ws, ho, by simp [hws, applyAll, applyWrite, Snapshot.toSt, s1, bk1, vecOf]⟩

end booking_kernel.Clock
