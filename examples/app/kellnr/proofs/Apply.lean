import Lemmas
import H5iAppLib
/-!
# Committing a write set

The kernel's `apply` computes `Spec.applyAll`. Loops use `H5iAppLib`'s
`loop_search` (`set_yanked`) and `loop_fold` (`del_pair`, `apply`).
-/
open Aeneas Aeneas.Std Result kellnr_kernel kellnr_kernel.Spec H5iAppLib

namespace kellnr_kernel.ApplyProofs

/-- Rows the kernel writes; each write adds at most one. -/
def total (s : Snapshot) : Nat :=
  s.crates.length + s.versions.length + s.owners.length + s.crate_users.length + s.crate_groups.length

theorem snapshot_clone (s : Snapshot) : Snapshot.Insts.CoreCloneClone.clone s = ok s := by
  simp [Snapshot.Insts.CoreCloneClone.clone, Settings.Insts.CoreCloneClone.clone, lift,
    vec_clone_eq User.Insts.CoreCloneClone s.users (fun _ => rfl),
    vec_clone_eq Krate.Insts.CoreCloneClone s.crates (fun _ => rfl),
    vec_clone_eq Version.Insts.CoreCloneClone s.versions (fun _ => rfl),
    vec_clone_eq Pair.Insts.CoreCloneClone s.owners (fun _ => rfl),
    vec_clone_eq Pair.Insts.CoreCloneClone s.crate_users (fun _ => rfl),
    vec_clone_eq Pair.Insts.CoreCloneClone s.crate_groups (fun _ => rfl),
    vec_clone_eq Pair.Insts.CoreCloneClone s.group_members (fun _ => rfl)]

/-! ## Table loops -/

@[step]
theorem put_pair_spec (v : alloc.vec.Vec Pair) (x : Pair) (hroom : v.length < Usize.max) :
    put_pair v x ⦃ v' => v'.val = putPair v.val x ∧ v'.length ≤ v.length + 1 ⦄ := by
  unfold put_pair putPair
  step*
  all_goals simp_all

@[step]
theorem del_pair_loop_spec (v : alloc.vec.Vec Pair) (x : Pair) (out : alloc.vec.Vec Pair)
    (i : Usize) (hi : i.val ≤ v.length)
    (hout : out.val = (v.val.take i.val).filter
      (fun o => decide ¬(o.a.val = x.a.val ∧ o.b.val = x.b.val))) :
    del_pair_loop v x out i ⦃ v' => v'.val = delPair v.val x ∧ v'.length ≤ v.length ⦄ := by
  unfold del_pair_loop
  apply WP.spec_mono (loop_fold v.val (fun w : alloc.vec.Vec Pair => w.val)
    (fun acc y => if decide ¬(y.a.val = x.a.val ∧ y.b.val = x.b.val) then acc ++ [y] else acc)
    (fun w j => w.length ≤ j) (fun y => del_pair_loop.body v x y.1 y.2) ?_ out i hi
    (by rw [alloc.vec.Vec.length, hout]; exact (List.length_filter_le _ _).trans (List.length_take_le _ _)))
  · intro r hr
    have e : r.val = delPair v.val x := by rw [hr, foldl_filter, hout, filter_split]; rfl
    exact ⟨e, by simp only [alloc.vec.Vec.length, e, delPair]; exact List.length_filter_le _ _⟩
  · intro o j hj ho; have := v.len_ineq; unfold del_pair_loop.body; h5i_step

/-- `set_yanked`'s search, from index `k`. -/
theorem searchFrom_set (l : List Version) (x : Version) (k : Nat) (hk : k ≤ l.length) :
    searchFrom l (fun v => decide (v.krate.val = x.krate.val ∧ v.vers.val = x.vers.val))
      (fun j _ => l.set j x) l k = l.take k ++ setYanked (l.drop k) x := by
  induction h : l.length - k generalizing k with
  | zero =>
    have : k = l.length := by omega
    subst this
    rw [searchFrom_end (le_refl _)]; simp [setYanked]
  | succ n ih =>
    have hk' : k < l.length := by omega
    rw [List.drop_eq_getElem_cons hk']
    by_cases hp : l[k].krate.val = x.krate.val ∧ l[k].vers.val = x.vers.val
    · rw [searchFrom_found hk' (by simpa using hp)]; simp only [setYanked, hp, and_self, if_true]
      rw [List.set_eq_take_append_cons_drop, if_pos hk']
    · rw [searchFrom_skip hk' (by simpa using hp), ih (k + 1) (by omega) (by omega)]
      simp only [setYanked, hp, if_false]
      rw [List.take_add_one, List.getElem?_eq_getElem hk', Option.toList_some, List.append_assoc]
      rfl

@[step]
theorem set_yanked_spec (v : alloc.vec.Vec Version) (x : Version) :
    set_yanked v x ⦃ v' => v'.val = setYanked v.val x ∧ v'.length = v.length ⦄ := by
  unfold set_yanked set_yanked_loop
  apply WP.spec_mono (loop_search v.val
    (fun y => decide (y.krate.val = x.krate.val ∧ y.vers.val = x.vers.val))
    (fun y : alloc.vec.Vec Version => y.val) (fun j _ => v.val.set j x) v.val _ ?_ 0#usize (by simp))
  · intro r hr
    have hs := searchFrom_set v.val x 0 (by simp)
    simp only [List.take_zero, List.drop_zero, List.nil_append] at hs
    have hr' : r.val = setYanked v.val x := by rw [hr, ← hs]; rfl
    refine ⟨hr', ?_⟩
    simp only [alloc.vec.Vec.length, hr']
    clear hr hr' hs
    induction v.val with
    | nil => rfl
    | cons y ys ih => simp only [setYanked]; split <;> simp [ih]
  · intro j hj; unfold set_yanked_loop.body; h5i_step

/-! ## One write, then the whole set -/

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
  · intro t j hj ht
    unfold total at ht
    unfold apply_loop.body
    step*
    all_goals (try (cases w <;> step*))
    all_goals try simp only [FoldStep]
    all_goals first
      | (simp; done)
      | exact ⟨by scalar_tac, trivial⟩
      | refine ⟨by scalar_tac, ?_, i2_post, ?_⟩
        · rw [← w_post]; simp [Snapshot.toSt, applyWrite, *]
        · simp only [total, alloc.vec.Vec.length] at *; scalar_tac

/-- The kernel's `apply` computes `Spec.applyAll`, as long as no vector can
overflow (each write adds at most one row). -/
theorem apply_spec (s : Snapshot) (ws : alloc.vec.Vec Write) (h : total s + ws.length < Usize.max) :
    apply s ws ⦃ s' => Snapshot.toSt s' = applyAll (Snapshot.toSt s) ws.val ⦄ := by
  unfold apply
  rw [snapshot_clone]
  simp only [bind_ok]
  exact apply_loop_spec ws s s 0#usize (by simp) (by simp [applyAll]) (by scalar_tac)

end kellnr_kernel.ApplyProofs
