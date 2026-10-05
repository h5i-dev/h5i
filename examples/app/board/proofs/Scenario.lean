import Theorems
/-!
# A concrete run

A real run meets the theorems' hypotheses: Alice appoints herself and posts
"hi". Then Bob cannot delete her post, and she cannot demote herself, the
last moderator.
-/
open Aeneas Aeneas.Std Result board_kernel board_kernel.Spec board_kernel.Commands board_kernel.Theorems H5iAppLib

namespace board_kernel.Scenario

def alice : Principal := ⟨0#u64, 1#u64⟩
def bob : Principal := ⟨0#u64, 2#u64⟩
def hi : alloc.vec.Vec U8 := alloc.vec.Vec.from [104#u8, 105#u8] (by scalar_tac)

/-- The empty board. -/
def s0 : Snapshot := ⟨⟨0#u64⟩, alloc.vec.Vec.new Post, alloc.vec.Vec.new Moderator⟩
/-- Alice is the only moderator. -/
def s1 : Snapshot := ⟨⟨0#u64⟩, alloc.vec.Vec.new Post, alloc.vec.Vec.from [⟨1#u64⟩] (by scalar_tac)⟩
/-- Alice has also posted "hi". -/
def s2 : Snapshot :=
  ⟨⟨1#u64⟩, alloc.vec.Vec.from [⟨0#u64, 1#u64, hi⟩] (by scalar_tac), alloc.vec.Vec.from [⟨1#u64⟩] (by scalar_tac)⟩

theorem run {a s c r} {P : alloc.vec.Vec Write → Prop}
    (h : transition a s c ⦃ o => ∃ ws, o = .Ok (ws, r) ∧ P ws ⦄) :
    ∃ ws, transition a s c = ok (.Ok (ws, r)) ∧ P ws := by
  obtain ⟨o, ho, ws, rfl, hp⟩ := (WP.spec_equiv_exists _ _).1 h
  exact ⟨ws, ho, hp⟩

/-- On an empty board, Alice appoints herself. -/
theorem promote_first :
    transition alice s0 (.Promote 1#u64) ⦃ r => ∃ ws, r = .Ok (ws, .Done) ∧ ws.val = [.PutModerator ⟨1#u64⟩] ⦄ := by
  simp only [transition]; unfold promote
  have h0 : s0.moderators.len = 0#usize := by
    have : s0.moderators.length = 0 := by simp [s0]
    scalar_tac
  simp only [h0, if_true]
  repeat' (first | step | split)
  all_goals simp_all [s0, alice]

/-- Alice posts "hi" with id 0. -/
theorem publish_hi : transition alice s1 (.Publish hi) ⦃ r => ∃ ws, r = .Ok (ws, .Created 0#u64) ∧
    ws.val = [.PutPost ⟨0#u64, 1#u64, hi⟩, .SetCounter ⟨1#u64⟩] ⦄ := by
  simp only [transition]; unfold publish
  repeat' (first | step | split)
  all_goals simp_all [s1, alice, hi, textOk, core.num.U64.MAX, U64.rMax]
  · scalar_tac
  · exact (u64_val_eq _ _).1 (by simpa using i_post)

/-- The board reaches `s2`. -/
theorem s2_reachable : Reachable (Snapshot.toSt s2) := by
  have h0 : Reachable (Snapshot.toSt s0) := by
    have : Snapshot.toSt s0 = init := by simp [Snapshot.toSt, s0, init]
    rw [this]; exact .init
  obtain ⟨ws, ht, hws⟩ := run promote_first
  have h1 : Reachable (Snapshot.toSt s1) := by
    have := Reachable.step h0 ht
    rwa [hws, show applyAll (Snapshot.toSt s0) [.PutModerator ⟨1#u64⟩] = Snapshot.toSt s1 by
      simp [applyAll, applyWrite, Snapshot.toSt, s0, s1, upsert]] at this
  obtain ⟨ws, ht, hws⟩ := run publish_hi
  have := Reachable.step h1 ht
  rwa [hws, show applyAll (Snapshot.toSt s1) [.PutPost ⟨0#u64, 1#u64, hi⟩, .SetCounter ⟨1#u64⟩] =
    Snapshot.toSt s2 by simp [applyAll, applyWrite, Snapshot.toSt, s1, s2, upsert]] at this

/-- Bob is neither the author nor a moderator. -/
theorem bob_cannot_delete : transition bob s2 (.Delete 0#u64) = ok (.Err .Forbidden) := by
  apply eq_ok_of_spec
  simp only [transition]; unfold delete
  repeat' (first | step | split)
  all_goals simp_all [s2, bob]

/-- The last moderator stays. -/
theorem last_moderator_stays : transition alice s2 (.Demote 1#u64) = ok (.Err .LastModerator) := by
  apply eq_ok_of_spec
  simp only [transition]; unfold demote
  repeat' (first | step | split)
  all_goals simp_all [s2, alice]

end board_kernel.Scenario
