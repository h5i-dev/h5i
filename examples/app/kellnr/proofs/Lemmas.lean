import Spec
/-! Each extracted helper computes its list counterpart in `Spec`. -/
open Aeneas Aeneas.Std Result kellnr_kernel kellnr_kernel.Spec

namespace kellnr_kernel.Lemmas

theorem usize_max_le : Usize.max ≤ U64.max := by
  rw [Usize.max_def, U64.max_def]
  cases System.Platform.numBits_eq <;> simp_all [Usize.numBits, U64.numBits]

/-- Loop invariant shape: the answer on the unread suffix is unchanged. -/
theorem drop_cons {α} (l : List α) (j : Nat) (h : j < l.length) :
    l.drop j = l[j] :: l.drop (j + 1) := List.drop_eq_getElem_cons h

@[step]
theorem find_user_loop_spec (us : alloc.vec.Vec User) (u : U64) (i : Usize) (hi : i.val ≤ us.length) :
    find_user_loop us u i ⦃ r => r = (us.val.drop i.val).find? (·.id.val = u.val) ⦄ := by
  unfold find_user_loop
  apply loop.spec_decr_nat (measure := fun (j : Usize) => us.length - j.val)
    (inv := fun j => j.val ≤ us.length ∧
      (us.val.drop i.val).find? (·.id.val = u.val) = (us.val.drop j.val).find? (·.id.val = u.val))
  · rintro j ⟨hj, heq⟩
    unfold find_user_loop.body
    step*
    · rw [heq, drop_cons _ _ (by scalar_tac), List.find?_cons]; simp_all
    · refine ⟨by scalar_tac, ?_, by scalar_tac⟩
      rw [heq, drop_cons _ _ (by scalar_tac), List.find?_cons, i2_post]; simp_all
    · rw [heq, List.drop_eq_nil_of_le (by scalar_tac)]; rfl
  · simp [hi]

@[step]
theorem find_user_spec (us : alloc.vec.Vec User) (u : U64) :
    find_user us u ⦃ r => r = us.val.find? (·.id.val = u.val) ⦄ := by
  unfold find_user; step*; simp_all

@[step]
theorem find_crate_loop_spec (cs : alloc.vec.Vec Krate) (k : U64) (i : Usize) (hi : i.val ≤ cs.length) :
    find_crate_loop cs k i ⦃ r => r = (cs.val.drop i.val).find? (·.id.val = k.val) ⦄ := by
  unfold find_crate_loop
  apply loop.spec_decr_nat (measure := fun (j : Usize) => cs.length - j.val)
    (inv := fun j => j.val ≤ cs.length ∧
      (cs.val.drop i.val).find? (·.id.val = k.val) = (cs.val.drop j.val).find? (·.id.val = k.val))
  · rintro j ⟨hj, heq⟩
    unfold find_crate_loop.body
    step*
    · rw [heq, drop_cons _ _ (by scalar_tac), List.find?_cons]; simp_all
    · refine ⟨by scalar_tac, ?_, by scalar_tac⟩
      rw [heq, drop_cons _ _ (by scalar_tac), List.find?_cons, i2_post]; simp_all
    · rw [heq, List.drop_eq_nil_of_le (by scalar_tac)]; rfl
  · simp [hi]

@[step]
theorem find_crate_spec (cs : alloc.vec.Vec Krate) (k : U64) :
    find_crate cs k ⦃ r => r = cs.val.find? (·.id.val = k.val) ⦄ := by
  unfold find_crate; step*; simp_all

@[step]
theorem find_version_loop_spec (vs : alloc.vec.Vec Version) (k v : U64) (i : Usize)
    (hi : i.val ≤ vs.length) :
    find_version_loop vs k v i ⦃ r =>
      r = (vs.val.drop i.val).find? (fun x => x.krate.val = k.val ∧ x.vers.val = v.val) ⦄ := by
  unfold find_version_loop
  apply loop.spec_decr_nat (measure := fun (j : Usize) => vs.length - j.val)
    (inv := fun j => j.val ≤ vs.length ∧
      (vs.val.drop i.val).find? (fun x => x.krate.val = k.val ∧ x.vers.val = v.val) =
        (vs.val.drop j.val).find? (fun x => x.krate.val = k.val ∧ x.vers.val = v.val))
  · rintro j ⟨hj, heq⟩
    unfold find_version_loop.body
    step*
    all_goals first
      | (rw [heq, drop_cons _ _ (by scalar_tac), List.find?_cons]; simp_all; done)
      | (refine ⟨by scalar_tac, ?_, by scalar_tac⟩
         rw [heq, drop_cons _ _ (by scalar_tac), List.find?_cons, i2_post]; simp_all)
      | (rw [heq, List.drop_eq_nil_of_le (by scalar_tac)]; rfl)
  · simp [hi]

@[step]
theorem find_version_spec (vs : alloc.vec.Vec Version) (k v : U64) :
    find_version vs k v ⦃ r => r = vs.val.find? (fun x => x.krate.val = k.val ∧ x.vers.val = v.val) ⦄ := by
  unfold find_version; step*; simp_all

@[step]
theorem has_pair_loop_spec (ps : alloc.vec.Vec Pair) (a b : U64) (i : Usize) (hi : i.val ≤ ps.length) :
    has_pair_loop ps a b i ⦃ r => r = hasPair (ps.val.drop i.val) a.val b.val ⦄ := by
  unfold has_pair_loop
  apply loop.spec_decr_nat (measure := fun (j : Usize) => ps.length - j.val)
    (inv := fun j => j.val ≤ ps.length ∧
      hasPair (ps.val.drop i.val) a.val b.val = hasPair (ps.val.drop j.val) a.val b.val)
  · rintro j ⟨hj, heq⟩
    unfold has_pair_loop.body
    step*
    all_goals first
      | (rw [heq, drop_cons _ _ (by scalar_tac)]; simp only [hasPair, List.any_cons]; subst p_post; simp_all; done)
      | (refine ⟨by scalar_tac, ?_, by scalar_tac⟩
         rw [heq, drop_cons _ _ (by scalar_tac), i2_post]; simp only [hasPair, List.any_cons]
         subst p_post; simp_all)
      | (rw [heq, List.drop_eq_nil_of_le (by scalar_tac)]; rfl)
  · simp [hi]

@[step]
theorem has_pair_spec (ps : alloc.vec.Vec Pair) (a b : U64) :
    has_pair ps a b ⦃ r => r = hasPair ps.val a.val b.val ⦄ := by
  unfold has_pair; step*; simp_all

theorem count_cons (x : Pair) (l : List Pair) (a : Nat) :
    (List.filter (·.a.val = a) (x :: l)).length =
      (if x.a.val = a then 1 else 0) + (List.filter (·.a.val = a) l).length := by
  simp only [List.filter_cons]
  split <;> simp_all; omega

@[step]
theorem count_a_loop_spec (ps : alloc.vec.Vec Pair) (a n : U64) (i : Usize)
    (hi : i.val ≤ ps.length) (hn : n.val ≤ i.val) :
    count_a_loop ps a n i ⦃ r =>
      r.val = n.val + (List.filter (·.a.val = a.val) (ps.val.drop i.val)).length ⦄ := by
  unfold count_a_loop
  apply loop.spec_decr_nat (measure := fun (x : U64 × Usize) => ps.length - x.2.val)
    (inv := fun x => x.2.val ≤ ps.length ∧ x.1.val ≤ x.2.val ∧
      n.val + (List.filter (·.a.val = a.val) (ps.val.drop i.val)).length =
        x.1.val + (List.filter (·.a.val = a.val) (ps.val.drop x.2.val)).length)
  · rintro ⟨k, j⟩ ⟨hj, hk, heq⟩
    simp only at hj hk heq ⊢
    unfold count_a_loop.body
    step*
    · have hc : ps.val.drop j.val = p :: ps.val.drop (j.val + 1) := by
        rw [p_post]; exact drop_cons _ _ (by scalar_tac)
      have hlen := ps.len_ineq
      have hb := usize_max_le
      split
      · step*
        all_goals first
          | scalar_tac
          | refine ⟨by scalar_tac, by scalar_tac, ?_, by scalar_tac⟩
            rw [heq, hc, count_cons, i2_post]; grind
      · step*
        refine ⟨by scalar_tac, by scalar_tac, ?_, by scalar_tac⟩
        rw [heq, hc, count_cons, i2_post]; grind
    · rw [heq, List.drop_eq_nil_of_le (by scalar_tac)]; simp
  · simp [hi, hn]

@[step]
theorem count_a_spec (ps : alloc.vec.Vec Pair) (a : U64) :
    count_a ps a ⦃ n => n.val = (List.filter (·.a.val = a.val) ps.val).length ⦄ := by
  unfold count_a; step*; simp_all

@[step]
theorem is_crate_group_user_loop_spec (s : Snapshot) (k u : U64) (i : Usize)
    (hi : i.val ≤ s.crate_groups.length) :
    is_crate_group_user_loop s k u i ⦃ r =>
      r = (s.crate_groups.val.drop i.val).any
        (fun g => g.a.val = k.val ∧ hasPair s.group_members.val g.b.val u.val) ⦄ := by
  unfold is_crate_group_user_loop
  apply loop.spec_decr_nat (measure := fun (j : Usize) => s.crate_groups.length - j.val)
    (inv := fun j => j.val ≤ s.crate_groups.length ∧
      (s.crate_groups.val.drop i.val).any
          (fun g => g.a.val = k.val ∧ hasPair s.group_members.val g.b.val u.val) =
        (s.crate_groups.val.drop j.val).any
          (fun g => g.a.val = k.val ∧ hasPair s.group_members.val g.b.val u.val))
  · rintro j ⟨hj, heq⟩
    unfold is_crate_group_user_loop.body
    step*
    all_goals first
      | (rw [heq, drop_cons _ _ (by scalar_tac)]; simp only [List.any_cons]; subst p_post; simp_all; done)
      | (refine ⟨by scalar_tac, ?_, by scalar_tac⟩
         rw [heq, drop_cons _ _ (by scalar_tac), i2_post]; simp only [List.any_cons]
         subst p_post; simp_all)
      | (rw [heq, List.drop_eq_nil_of_le (by scalar_tac)]; rfl)
  · simp [hi]

@[step]
theorem is_crate_group_user_spec (s : Snapshot) (k u : U64) :
    is_crate_group_user s k u ⦃ r => r = inGrantedGroup (Snapshot.toSt s) k.val u.val ⦄ := by
  unfold is_crate_group_user; step*; simp_all [inGrantedGroup, Snapshot.toSt]

end kellnr_kernel.Lemmas
