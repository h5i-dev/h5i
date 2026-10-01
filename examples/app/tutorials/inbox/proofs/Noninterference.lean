import Theorems
/-!
# Two runs with the same view

If two states look the same to user `u`, every command by `u` gives the same
writes, reply and error in both. Nothing outside `view` reaches `u`, not
even through an error code.
-/
open Aeneas Aeneas.Std Result inbox_kernel inbox_kernel.Spec inbox_kernel.Commands H5iAppLib

namespace inbox_kernel.Noninterference

/-! ## The helpers as equations -/

theorem is_blocked_ok (bs : alloc.vec.Vec Block) (o s : U64) :
    is_blocked bs o s = ok (bs.val.any (blockedBy o s)) := eq_ok_of_spec (is_blocked_spec bs o s)

theorem last_seq_ok (ms : alloc.vec.Vec Message) (f t : U64) :
    last_seq ms f t = ok (lastSeq ms.val f t) := eq_ok_of_spec (last_seq_spec ms f t)

theorem find_message_ok (ms : alloc.vec.Vec Message) (f t q : U64) :
    find_message ms f t q = ok (ms.val.find? (keyIs f t q)) := eq_ok_of_spec (find_message_spec ms f t q)

/-! ## Each helper gives the same answer in both states -/

section
variable (u : U64) (s₁ s₂ : Snapshot)
  (hv : view (Snapshot.toSt s₁) u.val = view (Snapshot.toSt s₂) u.val)
include hv

theorem msgs_same : s₁.messages.val.filter (involves u.val) = s₂.messages.val.filter (involves u.val) := by
  simpa [view, Snapshot.toSt] using congrArg Prod.fst hv

theorem blocks_same :
    s₁.blocks.val.filter (fun b => b.sender.val = u.val) = s₂.blocks.val.filter (fun b => b.sender.val = u.val) := by
  simpa [view, Snapshot.toSt] using congrArg Prod.snd hv

/-- Whether `d` blocks `u` is part of `u`'s view. -/
theorem is_blocked_same (d : U64) : is_blocked s₁.blocks d u = is_blocked s₂.blocks d u := by
  have hi : ∀ b, blockedBy d u b = true → decide (b.sender.val = u.val) = true := by
    intro b hb; simp only [blockedBy, Bool.and_eq_true, decide_eq_true_eq] at hb; simp [hb.2]
  rw [is_blocked_ok, is_blocked_ok, ← any_filter_of_imp _ _ _ (fun b _ => hi b), ← any_filter_of_imp s₂.blocks.val _ _ (fun b _ => hi b),
    blocks_same u s₁ s₂ hv]

theorem last_seq_same (d : U64) : last_seq s₁.messages u d = last_seq s₂.messages u d := by
  have hs : ∀ o m, involves u.val m = false → lastStep u d o m = o := by
    intro o m hm
    simp only [involves, Bool.or_eq_false_iff, decide_eq_false_iff_not] at hm
    simp [lastStep, hm.1]
  rw [last_seq_ok, last_seq_ok]
  simp only [lastSeq]
  rw [← foldl_filter_of_skip _ _ _ _ hs, ← foldl_filter_of_skip s₂.messages.val _ _ _ hs, msgs_same u s₁ s₂ hv]

theorem find_message_same (f t q : U64) (h : f = u ∨ t = u) :
    find_message s₁.messages f t q = find_message s₂.messages f t q := by
  have hi : ∀ m, keyIs f t q m = true → involves u.val m = true := by
    intro m hm
    simp only [keyIs, Bool.and_eq_true, decide_eq_true_eq] at hm
    rcases h with rfl | rfl <;> simp [involves, hm.1.1, hm.1.2]
  rw [find_message_ok, find_message_ok, ← find?_filter_of_imp _ _ _ (fun m _ => hi m),
    ← find?_filter_of_imp s₂.messages.val _ _ (fun m _ => hi m), msgs_same u s₁ s₂ hv]

theorem mailbox_same (inc : Bool) : mailbox s₁.messages u inc = mailbox s₂.messages u inc := by
  obtain ⟨v₁, e₁, h₁⟩ := Theorems.mailbox_ok s₁.messages u inc
  obtain ⟨v₂, e₂, h₂⟩ := Theorems.mailbox_ok s₂.messages u inc
  have hi : ∀ m, kept u inc m = true → involves u.val m = true := fun _ h => Theorems.kept_involves h
  rw [e₁, e₂, alloc.vec.Vec.ext v₁ v₂ (by
    rw [h₁, h₂, ← filter_filter_of_imp _ _ _ (fun m _ => hi m),
      ← filter_filter_of_imp s₂.messages.val _ _ (fun m _ => hi m),
      msgs_same u s₁ s₂ hv])]

end

/-- A user's outcome depends only on what that user may see: the writes,
the reply and the error are the same in any two states with the same view. -/
theorem noninterference (a : Principal) (s₁ s₂ : Snapshot) (c : Command)
    (hv : view (Snapshot.toSt s₁) a.user.val = view (Snapshot.toSt s₂) a.user.val) :
    transition a s₁ c = transition a s₂ c := by
  cases c with
  | Send d t =>
    simp only [transition, send, is_blocked_same a.user s₁ s₂ hv, last_seq_same a.user s₁ s₂ hv]
  | Inbox => simp only [transition, mailbox_same a.user s₁ s₂ hv]
  | Sent => simp only [transition, mailbox_same a.user s₁ s₂ hv]
  | MarkRead f q =>
    simp only [transition, mark_read, find_message_same a.user s₁ s₂ hv f a.user q (.inr rfl)]
  | Delete f t q =>
    simp only [transition, delete]
    by_cases h : f = a.user ∨ t = a.user
    · simp only [find_message_same a.user s₁ s₂ hv f t q h]
    · have hf : (f != a.user) = true := by simpa using fun e => h (.inl e)
      have ht : (t != a.user) = true := by simpa using fun e => h (.inr e)
      simp only [hf, ht, ite_true]
  | Block x => rfl
  | Unblock x => rfl

end inbox_kernel.Noninterference
