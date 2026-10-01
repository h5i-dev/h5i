import Spec
import H5iAppLib
/-! Each extracted helper computes its list counterpart in `Spec`. Loops use
the generic lemmas in `H5iAppLib`. -/
open Aeneas Aeneas.Std Result docs_kernel docs_kernel.Spec H5iAppLib

namespace docs_kernel.Lemmas

@[step]
theorem role_eq_spec (a b : Role) :
    Role.Insts.CoreCmpPartialEqRole.eq a b ⦃ r => r = decide (a = b) ⦄ := by
  cases a <;> cases b <;> simp [Role.Insts.CoreCmpPartialEqRole.eq, Role.read_discriminant, WP.spec_ok]

@[step]
theorem document_clone_spec (d : Document) :
    Document.Insts.CoreCloneClone.clone d ⦃ d' => d' = d ⦄ := by
  simp [Document.Insts.CoreCloneClone.clone, vec_clone_eq core.clone.CloneU8 _ (fun _ => rfl)]

@[step]
theorem role_of_spec (ms : alloc.vec.Vec Member) (p u : U64) :
    role_of ms p u ⦃ r => r = roleOf ms.val p.val u.val ⦄ := by
  unfold role_of role_of_loop
  apply WP.spec_mono (loop_search ms.val (fun m => decide (m.project.val = p.val ∧ m.user.val = u.val))
    id (fun _ m => some m.role) none _ ?_ 0#usize (by simp))
  · intro r hr; simp only [id] at hr; rw [hr, searchFrom_find]; simp [roleOf]
  · intro j hj; unfold role_of_loop.body; h5i_step

@[step]
theorem can_spec (s : Snapshot) (u p : U64) (a : Action) :
    can s u p a ⦃ b => b = allowed (Snapshot.toSt s) u.val p.val a ⦄ := by
  unfold can
  step*
  · subst o_post; rename_i h; simp [allowed, Snapshot.toSt, h]
  · subst o_post; rename_i h
    cases role <;> cases a <;> simp [allows, allowed, policy, Snapshot.toSt, h]

@[step]
theorem find_document_spec (ds : alloc.vec.Vec Document) (id : U64) :
    find_document ds id ⦃ r => r = findDoc ds.val id.val ⦄ := by
  unfold find_document find_document_loop
  apply WP.spec_mono (loop_search ds.val (fun d => decide (d.id.val = id.val))
    _root_.id (fun _ d => some d) none _ ?_ 0#usize (by simp))
  · intro r hr; simp only [_root_.id] at hr; rw [hr, searchFrom_find]; simp [findDoc]
  · intro j hj; unfold find_document_loop.body; h5i_step

@[step]
theorem count_owners_spec (ms : alloc.vec.Vec Member) (p : U64) :
    count_owners ms p ⦃ n => n.val = owners ms.val p.val ⦄ := by
  unfold count_owners count_owners_loop
  apply WP.spec_mono (loop_fold ms.val (fun n : U64 => n.val)
    (fun c m => c + if decide (m.project.val = p.val ∧ m.role = .Owner) then 1 else 0)
    (fun n k => n.val ≤ k) (fun x => count_owners_loop.body ms p x.1 x.2) ?_ 0#u64 0#usize
    (by simp) (by simp))
  · intro r hr; rw [hr, foldl_count]; simp [owners]
  · intro n j hj hn
    have := ms.len_ineq; have := usize_max_le
    unfold count_owners_loop.body; h5i_step

@[step]
theorem documents_in_spec (ds : alloc.vec.Vec Document) (p : U64) :
    documents_in ds p ⦃ v => v.val = ds.val.filter (fun d => d.project.val = p.val) ⦄ := by
  unfold documents_in documents_in_loop
  apply WP.spec_mono (loop_fold ds.val (fun v : alloc.vec.Vec Document => v.val)
    (fun acc d => if decide (d.project.val = p.val) then acc ++ [d] else acc)
    (fun v k => v.length ≤ k) (fun x => documents_in_loop.body ds p x.1 x.2) ?_ _ 0#usize
    (by simp) (by simp))
  · intro r hr; rw [hr, foldl_filter]; simp
  · intro o j hj ho
    have := ds.len_ineq
    unfold documents_in_loop.body; h5i_step

@[step]
theorem webhook_of_spec (hs : alloc.vec.Vec Webhook) (p : U64) :
    webhook_of hs p ⦃ r => r.map (·.val) = webhookOf hs.val p.val ⦄ := by
  unfold webhook_of webhook_of_loop
  apply WP.spec_mono (loop_search hs.val (fun w => decide (w.project.val = p.val))
    (fun r => r.map (·.val)) (fun _ w => some w.dest.val) none _ ?_ 0#usize (by simp))
  · intro r hr; rw [hr, searchFrom_find]; simp [webhookOf]
  · intro j hj; unfold webhook_of_loop.body; h5i_step

end docs_kernel.Lemmas
