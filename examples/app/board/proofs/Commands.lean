import Spec
/-!
# What each command does

List specs for the helpers, then one lemma per command: the writes it makes
when it succeeds. `Theorems.lean` uses only these lemmas.
-/
open Aeneas Aeneas.Std Result board_kernel board_kernel.Spec H5iAppLib

namespace board_kernel.Commands

attribute [simp] u64_val_eq

/-! ## Helpers -/

theorem post_clone (p : Post) : Post.Insts.CoreCloneClone.clone p = ok p := by
  simp [Post.Insts.CoreCloneClone.clone, u8vec_clone, lift]

@[step] theorem post_clone_spec (p : Post) : Post.Insts.CoreCloneClone.clone p ⦃ q => q = p ⦄ := by
  simp [post_clone]

@[step] theorem one_spec (w : Write) : one w ⦃ v => v.val = [w] ⦄ := by
  unfold one; step*

@[step] theorem text_ok_spec (t : alloc.vec.Vec U8) : text_ok t ⦃ b => b = true ↔ textOk t.val ⦄ := by
  unfold text_ok textOk
  step*
  all_goals (simp only [MAX_TEXT] at *; simp_all [alloc.vec.Vec.length]; try scalar_tac)

@[step] theorem find_post_spec (ps : alloc.vec.Vec Post) (id : U64) :
    find_post ps id ⦃ o => o = ps.val.find? (fun p => p.id.val = id.val) ⦄ := by
  unfold find_post find_post_loop
  apply WP.spec_mono (loop_search ps.val (fun p => decide (p.id = id)) (fun o : Option Post => o)
    (fun _ p => some p) none _ ?_ 0#usize (by simp))
  · intro r hr; rw [search_find _ _ _ hr]; simp
  · intro j hj; unfold find_post_loop.body; h5i_step

@[step] theorem is_moderator_spec (ms : alloc.vec.Vec Moderator) (u : U64) :
    is_moderator ms u ⦃ b => b = true ↔ ∃ m ∈ ms.val, m.user.val = u.val ⦄ := by
  unfold is_moderator is_moderator_loop
  apply WP.spec_mono (loop_search ms.val (fun m => decide (m.user = u)) (fun b : Bool => b)
    (fun _ _ => true) false _ ?_ 0#usize (by simp))
  · intro r hr
    rw [hr, searchFrom_const]
    simp
  · intro j hj; unfold is_moderator_loop.body; h5i_step

/-! ## Commands

What a successful run writes, and which facts made it succeed. Refusals write
nothing. -/

/-- A post with a fresh id by the caller, and the counter moved past it. -/
theorem publish_spec (u : U64) (s : Snapshot) (t : alloc.vec.Vec U8) :
    publish u s t ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → textOk t.val ∧
      ∃ c : Counter, c.next_id.val = s.counter.next_id.val + 1 ∧
        ws.val = [.PutPost ⟨s.counter.next_id, u, t⟩, .SetCounter c] ⦄ := by
  unfold publish
  step*
  -- Left after `step*`: `next_id + 1` does not overflow.
  simp only [core.num.U64.MAX, U64.rMax] at *; scalar_tac

theorem edit_spec (u : U64) (s : Snapshot) (id : U64) (t : alloc.vec.Vec U8) :
    edit u s id t ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → textOk t.val ∧
      ∃ q, findPost (Snapshot.toSt s) id.val = some q ∧ q.author = u ∧
        ws.val = [.PutPost ⟨id, u, t⟩] ⦄ := by
  unfold edit
  step*
  intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h
  have ha : p.author = u := by simpa using ‹¬(p.author != u) = true›
  have hf : findPost (Snapshot.toSt s) id.val = some p := by
    simp only [findPost, Snapshot.toSt]; rw [← o_post]; exact ‹o = some p›
  refine ⟨b_post.1 ‹_›, p, hf, ha, ?_⟩
  simp [v1_post, ha, v_post]

theorem delete_spec (u : U64) (s : Snapshot) (id : U64) :
    delete u s id ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      (∃ q, findPost (Snapshot.toSt s) id.val = some q ∧ (q.author = u ∨ isMod (Snapshot.toSt s) u.val)) ∧
        ws.val = [.DelPost id] ⦄ := by
  unfold delete
  step*
  all_goals
    intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h
  all_goals have hf : findPost (Snapshot.toSt s) id.val = some p := by simp only [findPost, Snapshot.toSt]; rw [← o_post]; exact ‹o = some p›
  · exact ⟨⟨p, hf, .inl ‹p.author = u›⟩, v_post⟩
  · exact ⟨⟨p, hf, .inr (by simpa [isMod, Snapshot.toSt] using b_post.1 ‹b = true›)⟩, v_post⟩

theorem promote_spec (u : U64) (s : Snapshot) (target : U64) :
    promote u s target ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      (isMod (Snapshot.toSt s) u.val ∨ ((Snapshot.toSt s).mods = [] ∧ target = u)) ∧
        ws.val = [.PutModerator ⟨target⟩] ⦄ := by
  unfold promote
  dsimp only
  split <;> step*
  all_goals
    intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h
  -- The caller is a moderator.
  all_goals try exact ⟨.inl (by simpa [isMod, Snapshot.toSt] using b_post.1 ‹b = true›), v_post⟩
  -- There is no moderator yet, and the caller appoints themselves.
  refine ⟨.inr ⟨?_, by simpa using ‹decide (target = u) = true›⟩, v_post⟩
  have := ‹s.moderators.len = 0#usize›
  simp only [Snapshot.toSt]
  exact List.eq_nil_of_length_eq_zero (by scalar_tac)

theorem demote_spec (u : U64) (s : Snapshot) (target : U64) :
    demote u s target ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      isMod (Snapshot.toSt s) u.val ∧ isMod (Snapshot.toSt s) target.val ∧
        2 ≤ (Snapshot.toSt s).mods.length ∧ ws.val = [.DelModerator target] ⦄ := by
  unfold demote
  step*
  intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h
  refine ⟨by simpa [isMod, Snapshot.toSt] using b_post.1 ‹b = true›,
    by simpa [isMod, Snapshot.toSt] using b1_post.1 ‹b1 = true›,
    by have := ‹¬s.moderators.len ≤ 1#usize›; simp only [Snapshot.toSt]; scalar_tac, v_post⟩

end board_kernel.Commands
