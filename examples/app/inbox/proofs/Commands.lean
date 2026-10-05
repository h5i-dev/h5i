import Spec
/-!
# What each command does

List specs for the helpers, then one lemma per command: what a successful
run writes and replies. The theorems use only these lemmas.
-/
open Aeneas Aeneas.Std Result inbox_kernel inbox_kernel.Spec H5iAppLib

namespace inbox_kernel.Commands

attribute [simp] u64_val_eq

/-! ## Helpers, as list functions -/

/-- The block `owner` holds against `sender`. -/
def blockedBy (owner sender : U64) (b : Block) : Bool := b.owner = owner && b.sender = sender

/-- The message with key `(f, t, q)`. -/
def keyIs (f t q : U64) (m : Message) : Bool := m.sender = f && m.recipient = t && m.seq = q

/-- One step of `last_seq`: the largest `seq` from `f` to `t` so far. -/
def lastStep (f t : U64) (o : Option U64) (m : Message) : Option U64 :=
  if m.sender = f && m.recipient = t then
    some (match o with
      | none => m.seq
      | some k => if m.seq > k then m.seq else k)
  else o

def lastSeq (ms : List Message) (f t : U64) : Option U64 := ms.foldl (lastStep f t) none

/-- The messages a mailbox listing keeps. -/
def kept (u : U64) (incoming : Bool) (m : Message) : Bool :=
  if incoming then m.recipient = u && !m.recipient_deleted else m.sender = u && !m.sender_deleted

theorem message_clone (m : Message) : Message.Insts.CoreCloneClone.clone m = ok m := by
  simp [Message.Insts.CoreCloneClone.clone, u8vec_clone, lift]

@[step] theorem message_clone_spec (m : Message) : Message.Insts.CoreCloneClone.clone m ⦃ q => q = m ⦄ := by
  simp [message_clone]

@[step] theorem one_spec (w : Write) : one w ⦃ v => v.val = [w] ⦄ := by
  unfold one; step*

theorem one_eq (w : Write) : one w = ok (alloc.vec.Vec.from [w] (by simp; scalar_tac)) :=
  eq_ok_of_spec (WP.spec_mono (one_spec w) (fun v hv => alloc.vec.Vec.ext _ _ (by simp [hv, alloc.vec.Vec.from_val])))

@[step] theorem text_ok_spec (t : alloc.vec.Vec U8) : text_ok t ⦃ b => b = true ↔ textOk t.val ⦄ := by
  unfold text_ok textOk
  step*
  all_goals (simp only [MAX_TEXT] at *; simp_all; try scalar_tac)

@[step] theorem is_blocked_spec (bs : alloc.vec.Vec Block) (o s : U64) :
    is_blocked bs o s ⦃ b => b = bs.val.any (blockedBy o s) ⦄ := by
  unfold is_blocked is_blocked_loop
  apply WP.spec_mono (loop_search bs.val (blockedBy o s) (fun b : Bool => b)
    (fun _ _ => true) false _ ?_ 0#usize (by simp))
  · intro r hr; exact search_any _ _ _ hr
  · intro j hj; unfold is_blocked_loop.body; h5i_step [blockedBy]

@[step] theorem last_seq_spec (ms : alloc.vec.Vec Message) (f t : U64) :
    last_seq ms f t ⦃ o => o = lastSeq ms.val f t ⦄ := by
  unfold last_seq last_seq_loop
  apply WP.spec_mono (loop_fold ms.val (fun o : Option U64 => o) (lastStep f t)
    (fun _ _ => True) (fun x => last_seq_loop.body ms f t x.1 x.2) ?_ none 0#usize (by simp) trivial)
  · intro r hr; rw [hr]; rfl
  · intro o j hj _
    unfold last_seq_loop.body
    h5i_step [lastStep]

@[step] theorem find_message_spec (ms : alloc.vec.Vec Message) (f t q : U64) :
    find_message ms f t q ⦃ o => o = ms.val.find? (keyIs f t q) ⦄ := by
  unfold find_message find_message_loop
  apply WP.spec_mono (loop_search ms.val (keyIs f t q) (fun o : Option Message => o)
    (fun _ m => some m) none _ ?_ 0#usize (by simp))
  · intro r hr; exact search_find _ _ _ hr
  · intro j hj; unfold find_message_loop.body; h5i_step [keyIs]

@[step] theorem mailbox_spec (ms : alloc.vec.Vec Message) (u : U64) (incoming : Bool) :
    mailbox ms u incoming ⦃ v => v.val = ms.val.filter (kept u incoming) ⦄ := by
  unfold mailbox mailbox_loop
  apply WP.spec_mono (loop_fold ms.val (fun w : alloc.vec.Vec Message => w.val)
    (fun acc x => if kept u incoming x then acc ++ [x] else acc)
    (fun w j => w.length ≤ j) (fun x => mailbox_loop.body ms u incoming x.1 x.2) ?_ _ 0#usize (by simp) (by simp))
  · intro r hr; rw [hr, foldl_filter]; simp
  · intro o j hj ho
    have := ms.len_ineq
    unfold mailbox_loop.body
    cases incoming <;> step* <;> (repeat' (first | step | split | simp only [bind_ok] at *)) <;>
      (try simp only [FoldStep]) <;> (try simp_all [kept]) <;> scalar_tac

/-! ## `last_seq` bounds every existing `seq` -/

/-- `none` below everything, `some k` as `k + 1`. -/
def rank : Option U64 → Nat
  | none => 0
  | some k => k.val + 1

theorem rank_step (f t : U64) (o : Option U64) (m : Message) :
    rank o ≤ rank (lastStep f t o m) ∧
      (m.sender = f → m.recipient = t → m.seq.val + 1 ≤ rank (lastStep f t o m)) := by
  unfold lastStep
  by_cases h : m.sender = f ∧ m.recipient = t
  · simp only [h, decide_true, Bool.and_self, ite_true, rank]
    cases o with
    | none => simp
    | some k => dsimp only; split <;> scalar_tac
  · have : (decide (m.sender = f) && decide (m.recipient = t)) = false := by
      simpa [Bool.and_eq_true] using h
    simp only [this, Bool.false_eq_true, ite_false, le_refl, true_and]
    intro hf ht; exact absurd ⟨hf, ht⟩ h

theorem rank_foldl (f t : U64) (l : List Message) (o : Option U64) :
    rank o ≤ rank (l.foldl (lastStep f t) o) ∧
      ∀ m ∈ l, m.sender = f → m.recipient = t → m.seq.val + 1 ≤ rank (l.foldl (lastStep f t) o) := by
  induction l generalizing o with
  | nil => simp
  | cons x xs ih =>
    obtain ⟨h1, h2⟩ := rank_step f t o x
    obtain ⟨h3, h4⟩ := ih (lastStep f t o x)
    refine ⟨Nat.le_trans h1 h3, ?_⟩
    intro m hm hf ht
    rcases List.mem_cons.1 hm with rfl | hm
    · exact Nat.le_trans (h2 hf ht) h3
    · exact h4 m hm hf ht

/-- Every message from `f` to `t` has a `seq` below `rank (lastSeq ...)`. -/
theorem lastSeq_bound (ms : List Message) (f t : U64) :
    ∀ m ∈ ms, m.sender = f → m.recipient = t → m.seq.val < rank (lastSeq ms f t) := by
  intro m hm hf ht
  have := (rank_foldl f t ms none).2 m hm hf ht
  unfold lastSeq; omega

/-! ## Commands

What a successful run writes and replies, and which facts made it succeed. -/

/-- A new message from the caller, with a `seq` above every earlier one to the same recipient. -/
theorem send_spec (u : U64) (s : Snapshot) (d : U64) (t : alloc.vec.Vec U8) :
    send u s d t ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → textOk t.val ∧
      s.blocks.val.any (blockedBy d u) = false ∧
      ∃ q : U64, (∀ m ∈ s.messages.val, m.sender = u → m.recipient = d → m.seq.val < q.val) ∧
        ws.val = [.PutMessage ⟨u, d, q, t, false, false, false⟩] ∧ rep = .Sent q ⦄ := by
  unfold send
  step*
  -- Left: `k + 1` cannot overflow, and the two successful cases.
  all_goals try (simp only [core.num.U64.MAX, U64.rMax] at *; scalar_tac)
  all_goals have hb := lastSeq_bound s.messages.val u d
  all_goals rw [← o_post] at hb
  all_goals
    intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h
    refine ⟨b_post.1 ‹_›, by simpa [b1_post] using ‹¬b1 = true›, ?_⟩
  · refine ⟨0#u64, fun m hm hf ht => ?_, by simp [v1_post, v_post], rfl⟩
    have := hb m hm hf ht
    simp [‹o = none›, rank] at this
  · refine ⟨seq, fun m hm hf ht => ?_, by simp [v1_post, v_post], rfl⟩
    have := hb m hm hf ht
    simp only [‹o = some k›, rank] at this
    scalar_tac

/-- A message the caller received, marked read. -/
theorem mark_read_spec (u : U64) (s : Snapshot) (f q : U64) :
    mark_read u s f q ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      ∃ m, s.messages.val.find? (keyIs f u q) = some m ∧ m.recipient_deleted = false ∧
        ws.val = [.PutMessage { m with read := true }] ∧ rep = .Done ⦄ := by
  unfold mark_read
  step*

/-- `m` hidden on `u`'s side, where `f` and `t` are its sender and recipient. -/
def hideFor (u f t : U64) (m : Message) : Message :=
  { m with sender_deleted := if f = u then true else m.sender_deleted,
           recipient_deleted := if t = u then true else m.recipient_deleted }

/-- A message the caller sent or received, hidden on the caller's side. -/
theorem delete_spec (u : U64) (s : Snapshot) (f t q : U64) :
    delete u s f t q ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → (f = u ∨ t = u) ∧
      ∃ m, s.messages.val.find? (keyIs f t q) = some m ∧
        ws.val = [.PutMessage (hideFor u f t m)] ∧ rep = .Done ⦄ := by
  unfold delete
  step*
  all_goals
    have hm : List.find? (keyIs f t q) s.messages.val = some m := by rw [← o_post]; assumption
    by_cases hf : f = u <;> by_cases ht : t = u <;>
      simp_all [one_eq, hideFor] <;> (repeat' split) <;> simp only [WP.spec_ok] <;>
      intro ws rep h <;> simp only [core.result.Result.Ok.injEq, Prod.mk.injEq, reduceCtorEq] at h <;>
      obtain ⟨rfl, rfl⟩ := h <;> exact ⟨m, o_post.symm, by simp [alloc.vec.Vec.from_val, *], rfl⟩

end inbox_kernel.Commands
