import Commands
/-!
# Theorems about the booking service

`writes_of` sums up `Commands.lean`: a successful command does one of five
things. The other theorems split on those five and never look at the code.
-/
open Aeneas Aeneas.Std Result booking_kernel booking_kernel.Spec booking_kernel.Commands H5iAppLib

namespace booking_kernel.Theorems

/-- What a successful command writes, in five cases. -/
theorem writes_of (a : Principal) (s : Snapshot) (c : Command) ws r
    (h : transition a s c = .ok (.Ok (ws, r))) :
    let st := Snapshot.toSt s
    -- List: nothing.
    ws.val = [] ∨
    -- AddAdmin: by an admin, or the caller alone when there is none.
    (∃ tg, (isAdmin st a.user.val ∨ (st.admins = [] ∧ tg = a.user)) ∧ ws.val = [.PutAdmin ⟨tg⟩]) ∨
    -- CreateRoom: by an admin, with the next id.
    (∃ d cnt, isAdmin st a.user.val ∧ cnt.next_id.val = st.next + 1 ∧
      ws.val = [.PutRoom ⟨s.counter.next_id, d⟩, .SetCounter cnt]) ∨
    -- Book: a new booking by the caller, and its notification.
    (∃ b rm cnt, b.id = s.counter.next_id ∧ b.user = a.user ∧ findRoom st b.room.val = some rm ∧
      a.now.val < b.start_at.val ∧ b.start_at.val < b.end_at.val ∧ (∀ c ∈ st.bookings, Compatible b c) ∧
      cnt.next_id.val = st.next + 1 ∧ ws.val = [.PutBooking b, .SetCounter cnt, .Emit ⟨rm.dest, .Booked, b⟩]) ∨
    -- Cancel: an existing booking, by an admin or by its owner before it starts.
    (∃ id b rm, findBooking st id.val = some b ∧
      (isAdmin st a.user.val ∨ (b.user = a.user ∧ a.now.val < b.start_at.val)) ∧
      findRoom st b.room.val = some rm ∧ ws.val = [.DelBooking id, .Emit ⟨rm.dest, .Cancelled, b⟩]) := by
  intro st
  cases c with
  | AddAdmin tg =>
    obtain ⟨ha, hws⟩ := post_of_ok (add_admin_spec a.user s tg) h ws r rfl
    exact .inr (.inl ⟨tg, ha, hws⟩)
  | CreateRoom d =>
    obtain ⟨ha, cnt, hc, hws⟩ := post_of_ok (create_room_spec a.user s d) h ws r rfl
    exact .inr (.inr (.inl ⟨d, cnt, ha, hc, hws⟩))
  | Book room t0 t1 =>
    obtain ⟨rm, hrm, hnow, hlt, hfree, cnt, hc, hws⟩ := post_of_ok (book_spec a s room t0 t1) h ws r rfl
    exact .inr (.inr (.inr (.inl ⟨_, rm, cnt, rfl, rfl, hrm, hnow, hlt, hfree, hc, hws⟩)))
  | Cancel id =>
    obtain ⟨b, rm, hb, hwho, hrm, hws⟩ := post_of_ok (cancel_spec a s id) h ws r rfl
    exact .inr (.inr (.inr (.inr ⟨id, b, rm, hb, hwho, hrm, hws⟩)))
  | List =>
    simp [transition, vec_clone_eq Booking.Insts.CoreCloneClone s.bookings (fun _ => rfl)] at h
    left
    rw [← h.1]
    rfl

/-- No command makes the kernel fail: no panic, overflow or bad index. -/
theorem transition_total (a : Principal) (s : Snapshot) (c : Command) : ∃ r, transition a s c = ok r := by
  cases c <;> simp only [transition]
  · exact ok_of (add_admin_spec _ _ _)
  · exact ok_of (create_room_spec _ _ _)
  · exact ok_of (book_spec _ _ _ _ _)
  · exact ok_of (cancel_spec _ _ _)
  · exact ⟨.Ok (alloc.vec.Vec.new Write, .Bookings s.bookings),
      by simp [vec_clone_eq Booking.Insts.CoreCloneClone s.bookings (fun _ => rfl)]⟩

/-! ## Permissions and destinations -/

/-- The policy allows every write of a successful command, judged against the
state before it and the shell's time. So every notification goes to the
room's registered destination. -/
theorem authorized (a : Principal) (s : Snapshot) (c : Command) ws r
    (h : transition a s c = .ok (.Ok (ws, r))) :
    ∀ w ∈ ws.val, allowed (Snapshot.toSt s) a.user.val a.now.val w := by
  rcases writes_of a s c ws r h with
    hws | ⟨tg, ha, hws⟩ | ⟨d, cnt, ha, hc, hws⟩ | ⟨b, rm, cnt, hid, hu, hrm, hnow, hlt, hfree, hc, hws⟩ |
    ⟨id, b, rm, hb, hwho, hrm, hws⟩
  · simp [hws]
  · rw [hws]; intro w hw
    simp only [List.mem_singleton] at hw
    subst hw
    exact ha.imp id (fun ⟨h1, h2⟩ => ⟨h1, by rw [h2]⟩)
  · rw [hws]; intro w hw
    simp only [List.mem_cons, List.not_mem_nil, or_false] at hw
    rcases hw with rfl | rfl
    · exact ⟨ha, rfl⟩
    · exact hc
  · -- Book: the booking, the counter, and the notification to the room's destination.
    rw [hws]; intro w hw
    simp only [List.mem_cons, List.not_mem_nil, or_false] at hw
    rcases hw with rfl | rfl | rfl
    · exact ⟨by rw [hu], by rw [hid]; rfl, by simp [hrm], hnow, hlt, hfree⟩
    · exact hc
    · simp [allowed, destOf, hrm]
  · -- Cancel: the deletion, and the notification to the room's destination.
    rw [hws]; intro w hw
    simp only [List.mem_cons, List.not_mem_nil, or_false] at hw
    rcases hw with rfl | rfl
    · exact ⟨b, hb, hwho.symm.imp (fun ⟨h1, h2⟩ => ⟨by rw [h1], h2⟩) (fun h => h)⟩
    · simp [allowed, destOf, hrm]

/-! ## Invariants -/

theorem findRoom_mem {s : St} {id : Nat} {r : Room} (h : findRoom s id = some r) :
    r ∈ s.rooms ∧ r.id.val = id :=
  ⟨List.mem_of_find?_eq_some h, by simpa using List.find?_some h⟩

theorem findBooking_mem {s : St} {id : Nat} {b : Booking} (h : findBooking s id = some b) :
    b ∈ s.bookings ∧ b.id.val = id :=
  ⟨List.mem_of_find?_eq_some h, by simpa using List.find?_some h⟩

theorem apart_comm {s₁ e₁ s₂ e₂ : Nat} : Apart s₁ e₁ s₂ e₂ → Apart s₂ e₂ s₁ e₁ :=
  Or.symm

theorem compatible_comm {b c : Booking} (h : Compatible b c) : Compatible c b :=
  fun hr => apart_comm (h hr.symm)

theorem inv_add_admin {s : St} (hi : Inv s) (m : Admin) :
    Inv { s with admins := upsert (·.user) m s.admins } where
  room_keys := hi.room_keys
  booking_keys := hi.booking_keys
  rooms_fresh := hi.rooms_fresh
  bookings_fresh := hi.bookings_fresh
  booked_room := hi.booked_room
  nonempty := hi.nonempty
  compatible := hi.compatible

/-- A room with the next id, and the counter moved past it. -/
theorem inv_create_room {s : St} (hi : Inv s) (r : Room) (n : Nat) (hid : r.id.val = s.next)
    (hn : n = s.next + 1) :
    Inv { s with rooms := upsert (·.id) r s.rooms, next := n } where
  room_keys := nodup_map_upsert (·.id) (·.id) (fun _ _ => Iff.rfl) r s.rooms hi.room_keys
  booking_keys := hi.booking_keys
  rooms_fresh z hz := by
    dsimp only at hz ⊢
    rcases mem_upsert_of hz with rfl | hz
    · omega
    · have := hi.rooms_fresh z hz; omega
  bookings_fresh z hz := by dsimp only at hz ⊢; have := hi.bookings_fresh z hz; omega
  booked_room b hb := by
    obtain ⟨x, hx, hxb⟩ := hi.booked_room b hb
    refine ⟨x, mem_upsert_of_ne hx fun he => ?_, hxb⟩
    have := hi.rooms_fresh x hx
    rw [he] at this; omega
  nonempty := hi.nonempty
  compatible := hi.compatible

/-- A new booking that passed the checks, and the counter moved past it. -/
theorem inv_book {s : St} (hi : Inv s) (b : Booking) (rm : Room) (n : Nat) (hid : b.id.val = s.next)
    (hrm : findRoom s b.room.val = some rm) (hlt : b.start_at.val < b.end_at.val)
    (hfree : ∀ c ∈ s.bookings, Compatible b c) (hn : n = s.next + 1) :
    Inv { s with bookings := upsert (·.id) b s.bookings, next := n } := by
  have happ : upsert (·.id) b s.bookings = s.bookings ++ [b] :=
    upsert_fresh _ _ _ fun y hy he => by
      have := hi.bookings_fresh y hy
      rw [show y.id = b.id from he] at this; omega
  obtain ⟨hrmem, hrid⟩ := findRoom_mem hrm
  refine ⟨hi.room_keys, ?_, fun z hz => by dsimp only at hz ⊢; have := hi.rooms_fresh z hz; omega, ?_, ?_, ?_, ?_⟩
  · exact nodup_map_upsert (·.id) (·.id) (fun _ _ => Iff.rfl) b s.bookings hi.booking_keys
  all_goals simp only [happ, List.mem_append, List.mem_singleton]
  · rintro z (hz | rfl)
    · have := hi.bookings_fresh z hz; omega
    · omega
  · rintro z (hz | rfl)
    · exact hi.booked_room z hz
    · exact ⟨rm, hrmem, (u64_val_eq _ _).1 hrid⟩
  · rintro z (hz | rfl)
    · exact hi.nonempty z hz
    · exact hlt
  · rw [List.pairwise_append]
    refine ⟨hi.compatible, List.pairwise_singleton _ _, ?_⟩
    intro c hc z hz
    rw [List.mem_singleton] at hz
    subst hz
    exact compatible_comm (hfree c hc)

theorem inv_cancel {s : St} (hi : Inv s) (id : U64) :
    Inv { s with bookings := s.bookings.filter (fun b => b.id ≠ id) } where
  room_keys := hi.room_keys
  booking_keys := nodup_map_filter _ _ _ hi.booking_keys
  rooms_fresh := hi.rooms_fresh
  bookings_fresh z hz := hi.bookings_fresh z (List.mem_filter.1 hz).1
  booked_room z hz := hi.booked_room z (List.mem_filter.1 hz).1
  nonempty z hz := hi.nonempty z (List.mem_filter.1 hz).1
  compatible := hi.compatible.sublist List.filter_sublist

/-- Successful commands keep the invariants, whatever the time. -/
theorem inv_preserved (a : Principal) (s : Snapshot) (c : Command) ws r
    (hinv : Inv (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r))) :
    Inv (applyAll (Snapshot.toSt s) ws.val) := by
  rcases writes_of a s c ws r h with
    hws | ⟨tg, ha, hws⟩ | ⟨d, cnt, ha, hc, hws⟩ | ⟨b, rm, cnt, hid, hu, hrm, hnow, hlt, hfree, hc, hws⟩ |
    ⟨id, b, rm, hb, hwho, hrm, hws⟩ <;> rw [hws] <;> simp only [applyAll, List.foldl_cons, List.foldl_nil, applyWrite]
  · exact hinv
  · exact inv_add_admin hinv _
  · exact inv_create_room hinv _ _ rfl hc
  · exact inv_book hinv b rm _ (by rw [hid]; rfl) hrm hlt hfree hc
  · exact inv_cancel hinv id

theorem init_inv : Inv init := by
  constructor <;> simp [init]

/-- The invariants hold in every state the service can reach. -/
theorem reachable_inv {s : St} (h : Reachable s) : Inv s := by
  induction h with
  | init => exact init_inv
  | step _ ht ih => exact inv_preserved _ _ _ _ _ ih ht

/-- For non-empty intervals, `Apart` means exactly that no instant lies in both. -/
theorem apart_iff {s₁ e₁ s₂ e₂ : Nat} (h₁ : s₁ < e₁) (h₂ : s₂ < e₂) :
    Apart s₁ e₁ s₂ e₂ ↔ ∀ t, ¬ (s₁ ≤ t ∧ t < e₁ ∧ s₂ ≤ t ∧ t < e₂) := by
  unfold Apart
  constructor
  · rintro h t ⟨_, _, _, _⟩; omega
  · intro h
    by_contra hn
    exact h (max s₁ s₂) ⟨Nat.le_max_left _ _, by omega, Nat.le_max_right _ _, by omega⟩

/-- No instant of any room is booked twice, in any reachable state. -/
theorem no_double_booking {s : St} (h : Reachable s) :
    ∀ b ∈ s.bookings, ∀ c ∈ s.bookings, b.id ≠ c.id → b.room = c.room →
      ∀ t, ¬ (b.start_at.val ≤ t ∧ t < b.end_at.val ∧ c.start_at.val ≤ t ∧ t < c.end_at.val) := by
  intro b hb c hc hid hroom t
  have hi := reachable_inv h
  haveI : Std.Symm Compatible := ⟨fun _ _ => compatible_comm⟩
  have hbc : Compatible b c := hi.compatible.forall hb hc (fun e => hid (e ▸ rfl))
  exact ((apart_iff (hi.nonempty b hb) (hi.nonempty c hc)).1 (hbc hroom)) t

/-! ## Notifications -/

/-- A notification goes to the room's destination and describes a change the
same write set commits: a new booking, or one that is now gone. -/
theorem effects_sound (a : Principal) (s : Snapshot) (c : Command) ws r
    (hinv : Inv (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r))) :
    let st := Snapshot.toSt s
    ∀ e, .Emit e ∈ ws.val →
      destOf st e.booking.room.val = some e.dest.val ∧
      (e.event = .Booked → e.booking ∉ st.bookings ∧ e.booking ∈ (applyAll st ws.val).bookings) ∧
      (e.event = .Cancelled → e.booking ∈ st.bookings ∧ e.booking ∉ (applyAll st ws.val).bookings) := by
  intro st e he
  have hdest := authorized a s c ws r h _ he
  refine ⟨hdest, ?_⟩
  rcases writes_of a s c ws r h with
    hws | ⟨tg, ha, hws⟩ | ⟨d, cnt, ha, hc, hws⟩ | ⟨b, rm, cnt, hid, hu, hrm, hnow, hlt, hfree, hc, hws⟩ |
    ⟨id, b, rm, hb, hwho, hrm, hws⟩ <;> rw [hws] at he ⊢ <;>
    simp only [List.mem_cons, List.not_mem_nil, or_false, reduceCtorEq, false_or, Write.Emit.injEq] at he
  · -- Book: the only notification is for the new booking.
    subst he
    simp only [applyAll, List.foldl_cons, List.foldl_nil, applyWrite, reduceCtorEq, false_implies, and_true,
      forall_const]
    refine ⟨fun hm => ?_, mem_upsert_self _ _ _⟩
    have := hinv.bookings_fresh _ hm
    rw [hid] at this
    exact Nat.lt_irrefl _ this
  · -- Cancel: the notification is for the booking the command deletes.
    subst he
    simp only [applyAll, List.foldl_cons, List.foldl_nil, applyWrite, reduceCtorEq, false_implies, true_and,
      forall_const]
    obtain ⟨hm, hbid⟩ := findBooking_mem hb
    refine ⟨hm, fun hf => ?_⟩
    have := (List.mem_filter.1 hf).2
    simp [(u64_val_eq _ _).1 hbid] at this

/-- Every booking and every cancellation comes with its notification. -/
theorem changes_announced (a : Principal) (s : Snapshot) (c : Command) ws r
    (h : transition a s c = .ok (.Ok (ws, r))) :
    (∀ b, .PutBooking b ∈ ws.val → ∃ d, .Emit ⟨d, .Booked, b⟩ ∈ ws.val) ∧
    (∀ id, .DelBooking id ∈ ws.val → ∃ d b, b.id = id ∧ .Emit ⟨d, .Cancelled, b⟩ ∈ ws.val) := by
  rcases writes_of a s c ws r h with
    hws | ⟨tg, ha, hws⟩ | ⟨d, cnt, ha, hc, hws⟩ | ⟨b, rm, cnt, hid, hu, hrm, hnow, hlt, hfree, hc, hws⟩ |
    ⟨id, b, rm, hb, hwho, hrm, hws⟩ <;> rw [hws] <;> simp
  exact (u64_val_eq _ _).1 (findBooking_mem hb).2

/-! ## Accepted commands

The theorems above would hold for a service that refuses everything. These
two show it accepts what the policy allows. -/

/-- A booking that satisfies the policy is accepted. -/
theorem book_accepted (a : Principal) (s : Snapshot) (room t0 t1 : U64)
    (hroom : findRoom (Snapshot.toSt s) room.val ≠ none) (hnow : a.now.val < t0.val) (hlt : t0.val < t1.val)
    (hfree : ∀ c ∈ (Snapshot.toSt s).bookings, room = c.room → Apart t0.val t1.val c.start_at.val c.end_at.val)
    (hid : s.counter.next_id.val < U64.max) :
    ∃ ws, transition a s (.Book room t0 t1) = ok (.Ok (ws, .Created s.counter.next_id)) := by
  obtain ⟨o, ho, ws, rfl⟩ := (WP.spec_equiv_exists _ _).1 (book_ok a s room t0 t1 hroom hnow hlt hfree hid)
  exact ⟨ws, ho⟩

/-- In a reachable state, an admin can cancel any booking, and its owner can
cancel it before it starts. -/
theorem cancel_accepted (a : Principal) (s : Snapshot) (id : U64) (b : Booking)
    (hr : Reachable (Snapshot.toSt s)) (hb : findBooking (Snapshot.toSt s) id.val = some b)
    (hwho : isAdmin (Snapshot.toSt s) a.user.val ∨ (b.user = a.user ∧ a.now.val < b.start_at.val)) :
    ∃ ws, transition a s (.Cancel id) = ok (.Ok (ws, .Done)) := by
  have hroom : findRoom (Snapshot.toSt s) b.room.val ≠ none := by
    obtain ⟨x, hx, hxb⟩ := (reachable_inv hr).booked_room b (findBooking_mem hb).1
    simp only [findRoom, ne_eq, List.find?_eq_none, not_forall, decide_eq_true_eq, not_not]
    exact ⟨x, hx, by rw [hxb]⟩
  obtain ⟨o, ho, ws, rfl⟩ := (WP.spec_equiv_exists _ _).1 (cancel_ok a s id b hb hwho hroom)
  exact ⟨ws, ho⟩

end booking_kernel.Theorems
