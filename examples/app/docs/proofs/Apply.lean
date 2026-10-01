import Spec
import H5iAppLib
/-! `apply` computes `Spec.applyAll`. Loops use the generic lemmas in `H5iAppLib`. -/
open Aeneas Aeneas.Std Result docs_kernel docs_kernel.Spec H5iAppLib

namespace docs_kernel.ApplyLemmas

/-- No vector can overflow: each write adds at most one row. -/
def Room (s : Snapshot) (n : Nat) : Prop :=
  s.projects.length + s.members.length + s.documents.length + s.webhooks.length + n < Usize.max

theorem project_clone (p : Project) : Project.Insts.CoreCloneClone.clone p = ok p := by
  simp [Project.Insts.CoreCloneClone.clone, u8vec_clone, lift]

theorem member_clone (m : Member) : Member.Insts.CoreCloneClone.clone m = ok m := by
  cases m; rename_i r; cases r <;> simp [Member.Insts.CoreCloneClone.clone, Role.Insts.CoreCloneClone.clone, lift]

theorem document_clone (d : Document) : Document.Insts.CoreCloneClone.clone d = ok d := by
  simp [Document.Insts.CoreCloneClone.clone, u8vec_clone]

theorem counter_clone (c : Counter) : Counter.Insts.CoreCloneClone.clone c = ok c := by
  simp [Counter.Insts.CoreCloneClone.clone, lift]

theorem webhook_clone (w : Webhook) : Webhook.Insts.CoreCloneClone.clone w = ok w := rfl

theorem effect_clone (e : Effect) : Effect.Insts.CoreCloneClone.clone e = ok e := rfl

theorem write_clone (w : Write) : Write.Insts.CoreCloneClone.clone w = ok w := by
  cases w <;> simp [Write.Insts.CoreCloneClone.clone, project_clone, member_clone,
    document_clone, counter_clone, webhook_clone, effect_clone, lift]

theorem snapshot_clone (s : Snapshot) : Snapshot.Insts.CoreCloneClone.clone s = ok s := by
  simp [Snapshot.Insts.CoreCloneClone.clone, counter_clone,
    vec_clone_eq Project.Insts.CoreCloneClone s.projects project_clone,
    vec_clone_eq Member.Insts.CoreCloneClone s.members member_clone,
    vec_clone_eq Document.Insts.CoreCloneClone s.documents document_clone,
    vec_clone_eq Webhook.Insts.CoreCloneClone s.webhooks webhook_clone]

@[step]
theorem member_clone_spec (m : Member) : Member.Insts.CoreCloneClone.clone m ⦃ m' => m' = m ⦄ := by
  rw [member_clone]; simp

@[step]
theorem document_clone_spec (d : Document) : Document.Insts.CoreCloneClone.clone d ⦃ d' => d' = d ⦄ := by
  rw [document_clone]; simp

@[step]
theorem put_project_loop_spec (v : alloc.vec.Vec Project) (p : Project) (i : Usize)
    (hi : i.val ≤ v.length) (hroom : v.length < Usize.max)
    (hpre : upsert (fun q => (q.id.val, 0)) p v.val =
      v.val.take i.val ++ upsert (fun q => (q.id.val, 0)) p (v.val.drop i.val)) :
    put_project_loop v p i ⦃ v' => v'.val = upsert (fun q => (q.id.val, 0)) p v.val ⦄ := by
  unfold put_project_loop
  apply WP.spec_mono (loop_search v.val (fun q => decide ((q.id.val, 0) = (p.id.val, 0)))
    (fun w : alloc.vec.Vec Project => w.val) (fun j _ => v.val.set j p) (v.val ++ [p]) _ ?_ i hi)
  · intro r hr; rw [hr]; exact upsert_loop_result (fun q : Project => (q.id.val, 0)) p _ _ hi hpre
  · intro j hj; unfold put_project_loop.body; h5i_step

@[step]
theorem put_member_loop_spec (v : alloc.vec.Vec Member) (m : Member) (i : Usize)
    (hi : i.val ≤ v.length) (hroom : v.length < Usize.max)
    (hpre : upsert (fun n => (n.project.val, n.user.val)) m v.val =
      v.val.take i.val ++ upsert (fun n => (n.project.val, n.user.val)) m (v.val.drop i.val)) :
    put_member_loop v m i ⦃ v' => v'.val = upsert (fun n => (n.project.val, n.user.val)) m v.val ⦄ := by
  unfold put_member_loop
  apply WP.spec_mono (loop_search v.val
    (fun n => decide ((n.project.val, n.user.val) = (m.project.val, m.user.val)))
    (fun w : alloc.vec.Vec Member => w.val) (fun j _ => v.val.set j m) (v.val ++ [m]) _ ?_ i hi)
  · intro r hr; rw [hr]
    exact upsert_loop_result (fun n : Member => (n.project.val, n.user.val)) m _ _ hi hpre
  · intro j hj; unfold put_member_loop.body; h5i_step

@[step]
theorem put_document_loop_spec (v : alloc.vec.Vec Document) (d : Document) (i : Usize)
    (hi : i.val ≤ v.length) (hroom : v.length < Usize.max)
    (hpre : upsert (fun e => (e.id.val, 0)) d v.val =
      v.val.take i.val ++ upsert (fun e => (e.id.val, 0)) d (v.val.drop i.val)) :
    put_document_loop v d i ⦃ v' => v'.val = upsert (fun e => (e.id.val, 0)) d v.val ⦄ := by
  unfold put_document_loop
  apply WP.spec_mono (loop_search v.val (fun e => decide ((e.id.val, 0) = (d.id.val, 0)))
    (fun w : alloc.vec.Vec Document => w.val) (fun j _ => v.val.set j d) (v.val ++ [d]) _ ?_ i hi)
  · intro r hr; rw [hr]; exact upsert_loop_result (fun e : Document => (e.id.val, 0)) d _ _ hi hpre
  · intro j hj; unfold put_document_loop.body; h5i_step

@[step]
theorem del_member_loop_spec (v : alloc.vec.Vec Member) (p u : U64) (out : alloc.vec.Vec Member)
    (i : Usize) (hi : i.val ≤ v.length)
    (hout : out.val = (v.val.take i.val).filter (fun n => ¬(n.project = p ∧ n.user = u))) :
    del_member_loop v p u out i ⦃ v' => v'.val = v.val.filter (fun n => ¬(n.project = p ∧ n.user = u)) ⦄ := by
  unfold del_member_loop
  apply WP.spec_mono (loop_fold v.val (fun w : alloc.vec.Vec Member => w.val)
    (fun acc n => if decide (¬(n.project = p ∧ n.user = u)) then acc ++ [n] else acc)
    (fun w k => w.length ≤ k) (fun x => del_member_loop.body v p u x.1 x.2) ?_ out i hi
    (by rw [alloc.vec.Vec.length, hout]; exact (List.length_filter_le _ _).trans (List.length_take_le _ _)))
  · intro r hr; rw [hr, foldl_filter, hout, filter_split]
  · intro o j hj ho; have := v.len_ineq; unfold del_member_loop.body; h5i_step

@[step]
theorem del_document_loop_spec (v : alloc.vec.Vec Document) (id : U64) (out : alloc.vec.Vec Document)
    (i : Usize) (hi : i.val ≤ v.length)
    (hout : out.val = (v.val.take i.val).filter (fun e => e.id ≠ id)) :
    del_document_loop v id out i ⦃ v' => v'.val = v.val.filter (fun e => e.id ≠ id) ⦄ := by
  unfold del_document_loop
  apply WP.spec_mono (loop_fold v.val (fun w : alloc.vec.Vec Document => w.val)
    (fun acc e => if decide (e.id ≠ id) then acc ++ [e] else acc)
    (fun w k => w.length ≤ k) (fun x => del_document_loop.body v id x.1 x.2) ?_ out i hi
    (by rw [alloc.vec.Vec.length, hout]; exact (List.length_filter_le _ _).trans (List.length_take_le _ _)))
  · intro r hr; rw [hr, foldl_filter, hout, filter_split]
  · intro o j hj ho; have := v.len_ineq; unfold del_document_loop.body; h5i_step

@[step]
theorem put_webhook_loop_spec (v : alloc.vec.Vec Webhook) (w : Webhook) (i : Usize)
    (hi : i.val ≤ v.length) (hroom : v.length < Usize.max)
    (hpre : upsert (fun h => (h.project.val, 0)) w v.val =
      v.val.take i.val ++ upsert (fun h => (h.project.val, 0)) w (v.val.drop i.val)) :
    put_webhook_loop v w i ⦃ v' => v'.val = upsert (fun h => (h.project.val, 0)) w v.val ⦄ := by
  unfold put_webhook_loop
  apply WP.spec_mono (loop_search v.val (fun h => decide ((h.project.val, 0) = (w.project.val, 0)))
    (fun x : alloc.vec.Vec Webhook => x.val) (fun j _ => v.val.set j w) (v.val ++ [w]) _ ?_ i hi)
  · intro r hr; rw [hr]; exact upsert_loop_result (fun h : Webhook => (h.project.val, 0)) w _ _ hi hpre
  · intro j hj; unfold put_webhook_loop.body; h5i_step

@[step]
theorem del_webhook_loop_spec (v : alloc.vec.Vec Webhook) (p : U64) (out : alloc.vec.Vec Webhook)
    (i : Usize) (hi : i.val ≤ v.length)
    (hout : out.val = (v.val.take i.val).filter (fun h => h.project ≠ p)) :
    del_webhook_loop v p out i ⦃ v' => v'.val = v.val.filter (fun h => h.project ≠ p) ⦄ := by
  unfold del_webhook_loop
  apply WP.spec_mono (loop_fold v.val (fun x : alloc.vec.Vec Webhook => x.val)
    (fun acc h => if decide (h.project ≠ p) then acc ++ [h] else acc)
    (fun x k => x.length ≤ k) (fun x => del_webhook_loop.body v p x.1 x.2) ?_ out i hi
    (by rw [alloc.vec.Vec.length, hout]; exact (List.length_filter_le _ _).trans (List.length_take_le _ _)))
  · intro r hr; rw [hr, foldl_filter, hout, filter_split]
  · intro o j hj ho; have := v.len_ineq; unfold del_webhook_loop.body; h5i_step

/-- Total rows in a snapshot. -/
def total (s : Snapshot) : Nat :=
  s.projects.length + s.members.length + s.documents.length + s.webhooks.length

@[step]
theorem apply_write_spec (s : Snapshot) (w : Write) (h : total s < Usize.max) :
    apply_write s w ⦃ s' => Snapshot.toSt s' = applyWrite (Snapshot.toSt s) w ∧ total s' ≤ total s + 1 ⦄ := by
  unfold total at h
  unfold apply_write
  cases w <;> step*
  all_goals first
    | refine ⟨by simp [Snapshot.toSt, applyWrite, *], ?_⟩
      simp only [total, alloc.vec.Vec.length, *]
      first
        | omega
        | (have := upsert_length (fun q : Project => (q.id.val, 0)) ‹_› s.projects.val; omega)
        | (have := upsert_length (fun n : Member => (n.project.val, n.user.val)) ‹_› s.members.val; omega)
        | (have := upsert_length (fun e : Document => (e.id.val, 0)) ‹_› s.documents.val; omega)
        | (have := upsert_length (fun h : Webhook => (h.project.val, 0)) ‹_› s.webhooks.val; omega)
        | grind [List.length_filter_le]
    | simp

@[step]
theorem write_clone_spec (w : Write) : Write.Insts.CoreCloneClone.clone w ⦃ w' => w' = w ⦄ := by
  rw [write_clone]; simp

@[step]
theorem snapshot_clone_spec (s : Snapshot) : Snapshot.Insts.CoreCloneClone.clone s ⦃ s' => s' = s ⦄ := by
  rw [snapshot_clone]; simp

@[step]
theorem apply_loop_spec (ws : alloc.vec.Vec Write) (s₀ s : Snapshot) (i : Usize)
    (hi : i.val ≤ ws.length)
    (hs : Snapshot.toSt s = applyAll (Snapshot.toSt s₀) (ws.val.take i.val))
    (hroom : total s + (ws.length - i.val) < Usize.max) :
    apply_loop ws s i ⦃ s' => Snapshot.toSt s' = applyAll (Snapshot.toSt s₀) ws.val ⦄ := by
  unfold apply_loop
  apply WP.spec_mono (loop_fold ws.val Snapshot.toSt applyWrite
    (fun t k => total t + (ws.length - k) < Usize.max) (fun x => apply_loop.body ws x.1 x.2) ?_ s i hi hroom)
  · intro r hr
    rw [hr, hs, applyAll, applyAll, ← List.foldl_append, List.take_append_drop]
  · intro t j hj ht; unfold apply_loop.body; h5i_step

@[step]
theorem apply_spec (s : Snapshot) (ws : alloc.vec.Vec Write) (h : Room s ws.length) :
    apply s ws ⦃ s' => Snapshot.toSt s' = applyAll (Snapshot.toSt s) ws.val ⦄ := by
  unfold apply
  rw [snapshot_clone]
  simp only [bind_ok]
  exact apply_loop_spec ws s s 0#usize (by simp) (by simp [applyAll])
    (by simp only [Room] at h; simp only [total]; scalar_tac)

end docs_kernel.ApplyLemmas
