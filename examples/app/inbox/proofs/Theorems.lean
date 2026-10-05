import Commands
/-!
# Theorems about one run

`writes_of` sums up `Commands.lean`: a successful command does one of six
things, and the theorems here split on those six. Noninterference, a two-run
theorem, is in `Noninterference.lean`.
-/
open Aeneas Aeneas.Std Result inbox_kernel inbox_kernel.Spec inbox_kernel.Commands H5iAppLib

namespace inbox_kernel.Theorems

theorem mailbox_ok (ms : alloc.vec.Vec Message) (u : U64) (inc : Bool) :
    ∃ v, mailbox ms u inc = ok v ∧ v.val = ms.val.filter (kept u inc) := by
  obtain ⟨v, hv, h⟩ := (WP.spec_equiv_exists _ _).1 (mailbox_spec ms u inc)
  exact ⟨v, hv, h⟩

/-- What a successful command writes and replies, in six cases. -/
theorem writes_of (a : Principal) (s : Snapshot) (c : Command) ws r
    (h : transition a s c = .ok (.Ok (ws, r))) :
    let st := Snapshot.toSt s
    let u := a.user
    -- Inbox and Sent: nothing, and a listing.
    (ws.val = [] ∧ ∃ inc v, v.val = st.msgs.filter (kept u inc) ∧ r = .Messages v) ∨
    -- Send: a new message from the caller, with a `seq` above the earlier ones.
    (∃ d t q, textOk t.val ∧ (∀ m ∈ st.msgs, m.sender = u → m.recipient = d → m.seq.val < q.val) ∧
      ws.val = [.PutMessage ⟨u, d, q, t, false, false, false⟩] ∧ r = .Sent q) ∨
    -- MarkRead: a message the caller received.
    (∃ f q m, st.msgs.find? (keyIs f u q) = some m ∧
      ws.val = [.PutMessage { m with read := true }] ∧ r = .Done) ∨
    -- Delete: a message the caller sent or received, hidden on the caller's side.
    (∃ f t q m, (f = u ∨ t = u) ∧ st.msgs.find? (keyIs f t q) = some m ∧
      ws.val = [.PutMessage (hideFor u f t m)] ∧ r = .Done) ∨
    -- Block and Unblock: the caller's own block.
    (∃ x, ws.val = [.PutBlock ⟨u, x⟩] ∧ r = .Done) ∨
    (∃ x, ws.val = [.DelBlock ⟨u, x⟩] ∧ r = .Done) := by
  intro st u
  cases c with
  | Send d t =>
    obtain ⟨ht, -, q, hq, hws, hr⟩ := post_of_ok (send_spec a.user s d t) h ws r rfl
    exact .inr (.inl ⟨d, t, q, ht, hq, hws, hr⟩)
  | Inbox =>
    obtain ⟨v, hv, hvv⟩ := mailbox_ok s.messages a.user true
    simp only [transition, hv, bind_ok, ok.injEq, core.result.Result.Ok.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, rfl⟩ := h
    exact .inl ⟨rfl, true, v, hvv, rfl⟩
  | Sent =>
    obtain ⟨v, hv, hvv⟩ := mailbox_ok s.messages a.user false
    simp only [transition, hv, bind_ok, ok.injEq, core.result.Result.Ok.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, rfl⟩ := h
    exact .inl ⟨rfl, false, v, hvv, rfl⟩
  | MarkRead f q =>
    obtain ⟨m, hm, -, hws, hr⟩ := post_of_ok (mark_read_spec a.user s f q) h ws r rfl
    exact .inr (.inr (.inl ⟨f, q, m, hm, hws, hr⟩))
  | Delete f t q =>
    obtain ⟨hu, m, hm, hws, hr⟩ := post_of_ok (delete_spec a.user s f t q) h ws r rfl
    exact .inr (.inr (.inr (.inl ⟨f, t, q, m, hu, hm, hws, hr⟩)))
  | Block x =>
    simp only [transition, one_eq, bind_ok, ok.injEq, core.result.Result.Ok.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, rfl⟩ := h
    exact .inr (.inr (.inr (.inr (.inl ⟨x, by simp [alloc.vec.Vec.from_val, u], rfl⟩))))
  | Unblock x =>
    simp only [transition, one_eq, bind_ok, ok.injEq, core.result.Result.Ok.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, rfl⟩ := h
    exact .inr (.inr (.inr (.inr (.inr ⟨x, by simp [alloc.vec.Vec.from_val, u], rfl⟩))))

/-- No command makes the kernel fail: no panic, overflow or bad index. -/
theorem transition_total (a : Principal) (s : Snapshot) (c : Command) : ∃ r, transition a s c = ok r := by
  cases c <;> simp only [transition]
  · exact ok_of (send_spec _ _ _ _)
  · obtain ⟨v, hv, -⟩ := mailbox_ok s.messages a.user true; simp [hv]
  · obtain ⟨v, hv, -⟩ := mailbox_ok s.messages a.user false; simp [hv]
  · exact ok_of (mark_read_spec _ _ _ _)
  · exact ok_of (delete_spec _ _ _ _ _)
  · simp [one_eq]
  · simp [one_eq]

/-! ## Confinement -/

theorem keyIs_of {f t q : U64} {ms : List Message} {m : Message} (h : ms.find? (keyIs f t q) = some m) :
    m ∈ ms ∧ m.sender = f ∧ m.recipient = t ∧ m.seq = q := by
  have := List.find?_some h
  simp only [keyIs, Bool.and_eq_true, decide_eq_true_eq] at this
  exact ⟨List.mem_of_find?_eq_some h, this.1.1, this.1.2, this.2⟩

theorem kept_involves {u : U64} {inc : Bool} {m : Message} (h : kept u inc m = true) :
    involves u.val m = true := by
  unfold kept at h
  unfold involves
  cases inc <;> simp_all

/-- Every message in a reply was sent or received by the caller. -/
theorem reply_confined (a : Principal) (s : Snapshot) (c : Command) ws ms
    (h : transition a s c = .ok (.Ok (ws, .Messages ms))) :
    ∀ m ∈ ms.val, involves a.user.val m := by
  rcases writes_of a s c ws _ h with
    ⟨-, inc, v, hv, hr⟩ | ⟨_, _, _, _, _, _, hr⟩ | ⟨_, _, _, _, _, hr⟩ | ⟨_, _, _, _, _, _, _, hr⟩ |
    ⟨_, _, hr⟩ | ⟨_, _, hr⟩ <;> simp only [reduceCtorEq, Reply.Messages.injEq] at hr
  subst hr
  intro m hm
  rw [hv] at hm
  exact kept_involves (List.mem_filter.1 hm).2

/-- Every write of a successful command touches only the caller's rows. -/
theorem writes_confined (a : Principal) (s : Snapshot) (c : Command) ws r
    (h : transition a s c = .ok (.Ok (ws, r))) :
    ∀ w ∈ ws.val, touches a.user.val w := by
  rcases writes_of a s c ws r h with
    ⟨hws, -⟩ | ⟨d, t, q, -, -, hws, -⟩ | ⟨f, q, m, hm, hws, -⟩ | ⟨f, t, q, m, hu, hm, hws, -⟩ |
    ⟨x, hws, -⟩ | ⟨x, hws, -⟩ <;> rw [hws] <;> intro w hw <;>
    simp only [List.mem_cons, List.not_mem_nil, or_false] at hw
  all_goals first | exact absurd hw id | subst hw
  · simp [touches, involves]
  · obtain ⟨-, -, hr, -⟩ := keyIs_of hm
    simp [touches, involves, hr]
  · obtain ⟨-, hs, hr, -⟩ := keyIs_of hm
    simp only [touches, involves, hideFor, hs, hr]
    rcases hu with rfl | rfl <;> simp
  · simp [touches]
  · simp [touches]

/-! ## Everyone else's rows stay as they were -/

theorem others_write (s : St) (u : Nat) (w : Write) (hw : touches u w) :
    others (applyWrite s w) u = others s u := by
  cases w with
  | PutMessage m =>
    simp only [touches] at hw
    simp only [others, applyWrite, Prod.mk.injEq, and_true]
    refine filter_upsert _ _ _ _ (by simp [hw]) (fun y hy => ?_)
    simp only [msgKey, Prod.mk.injEq] at hy
    simp only [involves, hy.1, hy.2.1] at hw ⊢
    simp [hw]
  | PutBlock b =>
    simp only [touches] at hw
    simp only [others, applyWrite, Prod.mk.injEq, true_and]
    refine filter_upsert _ _ _ _ (by simp [hw]) (fun y hy => ?_)
    simp only [blockKey, Prod.mk.injEq] at hy
    simp [hy.1, hw]
  | DelBlock b =>
    simp only [touches] at hw
    simp only [others, applyWrite, Prod.mk.injEq, true_and]
    refine filter_filter_of_imp _ _ _ (fun y _ hp => Bool.eq_true_of_not_eq_false (fun hy => ?_))
    simp only [blockKey, ne_eq, decide_eq_false_iff_not, Decidable.not_not, Prod.mk.injEq] at hy
    simp [hy.1, hw] at hp

theorem others_applyAll (s : St) (u : Nat) (ws : List Write) (hw : ∀ w ∈ ws, touches u w) :
    others (applyAll s ws) u = others s u := by
  induction ws generalizing s with
  | nil => rfl
  | cons w ws ih =>
    simp only [applyAll, List.foldl_cons] at ih ⊢
    rw [ih _ (fun x hx => hw x (List.mem_cons_of_mem _ hx)), others_write _ _ _ (hw w List.mem_cons_self)]

/-- A command leaves every row that does not belong to the caller as it was:
other people's messages and other people's blocks. -/
theorem others_unchanged (a : Principal) (s : Snapshot) (c : Command) ws r
    (h : transition a s c = .ok (.Ok (ws, r))) :
    others (applyAll (Snapshot.toSt s) ws.val) a.user.val = others (Snapshot.toSt s) a.user.val :=
  others_applyAll _ _ _ (writes_confined a s c ws r h)

/-! ## Sending never overwrites -/

/-- A sent message gets a key no existing message has. -/
theorem send_fresh (a : Principal) (s : Snapshot) (d : U64) (t : alloc.vec.Vec U8) ws r
    (h : transition a s (.Send d t) = .ok (.Ok (ws, r))) :
    ∃ m, ws.val = [.PutMessage m] ∧ ∀ m' ∈ (Snapshot.toSt s).msgs, msgKey m' ≠ msgKey m := by
  obtain ⟨-, -, q, hq, hws, -⟩ := post_of_ok (send_spec a.user s d t) h ws r rfl
  refine ⟨_, hws, fun m' hm' he => ?_⟩
  simp only [msgKey, Prod.mk.injEq] at he
  have := hq m' hm' he.1 he.2.1
  rw [he.2.2] at this
  exact Nat.lt_irrefl _ this

/-! ## Invariants -/

theorem inv_put_msg {s : St} (hi : Inv s) (m : Message) (ht : textOk m.text.val) :
    Inv { s with msgs := upsert msgKey m s.msgs } where
  msg_keys := nodup_map_upsert msgKey msgKey (fun _ _ => Iff.rfl) m s.msgs hi.msg_keys
  texts z hz := by
    rcases mem_upsert_of hz with rfl | hz
    · exact ht
    · exact hi.texts z hz
  block_keys := hi.block_keys

theorem inv_put_block {s : St} (hi : Inv s) (b : Block) :
    Inv { s with blocks := upsert blockKey b s.blocks } where
  msg_keys := hi.msg_keys
  texts := hi.texts
  block_keys := nodup_map_upsert blockKey blockKey (fun _ _ => Iff.rfl) b s.blocks hi.block_keys

theorem inv_del_block {s : St} (hi : Inv s) (b : Block) :
    Inv { s with blocks := s.blocks.filter (fun x => blockKey x ≠ blockKey b) } where
  msg_keys := hi.msg_keys
  texts := hi.texts
  block_keys := nodup_map_filter _ _ _ hi.block_keys

/-- Successful commands keep the invariants. -/
theorem inv_preserved (a : Principal) (s : Snapshot) (c : Command) ws r
    (hinv : Inv (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r))) :
    Inv (applyAll (Snapshot.toSt s) ws.val) := by
  rcases writes_of a s c ws r h with
    ⟨hws, -⟩ | ⟨d, t, q, ht, -, hws, -⟩ | ⟨f, q, m, hm, hws, -⟩ | ⟨f, t, q, m, hu, hm, hws, -⟩ |
    ⟨x, hws, -⟩ | ⟨x, hws, -⟩ <;> rw [hws] <;> simp only [applyAll, List.foldl_cons, List.foldl_nil, applyWrite]
  · exact hinv
  · exact inv_put_msg hinv _ ht
  · exact inv_put_msg hinv _ (hinv.texts m (keyIs_of hm).1)
  · exact inv_put_msg hinv _ (hinv.texts m (keyIs_of hm).1)
  · exact inv_put_block hinv _
  · exact inv_del_block hinv _

theorem init_inv : Inv init := by
  constructor <;> simp [init]

/-- The invariants hold in every state the inbox can reach. -/
theorem reachable_inv {s : St} (h : Reachable s) : Inv s := by
  induction h with
  | init => exact init_inv
  | step _ ht ih => exact inv_preserved _ _ _ _ _ ih ht

/-! ## Messages only move forward -/

theorem later_refl (m : Message) : Later m m := ⟨rfl, id, id, id⟩

theorem same_msg {s : St} (hi : Inv s) {m m' : Message} (hm : m ∈ s.msgs) (hm' : m' ∈ s.msgs)
    (h : msgKey m' = msgKey m) : m' = m :=
  List.inj_on_of_nodup_map hi.msg_keys hm' hm h

/-- No command changes the text of a message or clears its read or deleted
flags; in particular, a message deleted from someone's view stays deleted. -/
theorem msg_kept (a : Principal) (s : Snapshot) (c : Command) ws r
    (hr : Reachable (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r))) :
    ∀ m ∈ (Snapshot.toSt s).msgs, ∀ m' ∈ (applyAll (Snapshot.toSt s) ws.val).msgs,
      msgKey m' = msgKey m → Later m m' := by
  have hi := reachable_inv hr
  intro m hm m' hm' hk
  rcases writes_of a s c ws r h with
    ⟨hws, -⟩ | ⟨d, t, q, ht, hq, hws, -⟩ | ⟨f, q, m0, h0, hws, -⟩ | ⟨f, t, q, m0, hu, h0, hws, -⟩ |
    ⟨x, hws, -⟩ | ⟨x, hws, -⟩ <;> rw [hws] at hm' <;>
    simp only [applyAll, List.foldl_cons, List.foldl_nil, applyWrite] at hm'
  all_goals try (rw [same_msg hi hm hm' hk]; exact later_refl m)
  all_goals rcases (mem_upsert_iff hi.msg_keys).1 hm' with rfl | ⟨hm', -⟩
  all_goals try (rw [same_msg hi hm hm' hk]; exact later_refl m)
  · -- Send: the new key is fresh, so it is not `m`'s.
    simp only [msgKey, Prod.mk.injEq] at hk
    have := hq m hm hk.1.symm hk.2.1.symm
    rw [hk.2.2] at this
    exact absurd this (Nat.lt_irrefl _)
  · -- MarkRead: the found message is `m`; only `read` changes.
    obtain ⟨h0m, -⟩ := keyIs_of h0
    rw [same_msg hi hm h0m hk]
    exact ⟨rfl, fun _ => rfl, id, id⟩
  · -- Delete: the found message is `m`; only the deleted flags change.
    obtain ⟨h0m, -⟩ := keyIs_of h0
    rw [same_msg hi hm h0m hk]
    refine ⟨rfl, id, fun h => ?_, fun h => ?_⟩ <;> simp only [hideFor] <;> split <;> simp_all

end inbox_kernel.Theorems
