import H5iAppLib.Loops
/-! Keyed tables as lists. `upsert` models `INSERT ... ON CONFLICT (key) DO UPDATE`. -/
namespace H5iAppLib

/-- Replace the first row with `x`'s key by `x`, or append `x`. -/
def upsert {α} {κ : Type} [DecidableEq κ] (key : α → κ) (x : α) : List α → List α
  | [] => [x]
  | y :: ys => if key y = key x then x :: ys else y :: upsert key x ys

theorem upsert_eq {α} {κ : Type} [DecidableEq κ] (key : α → κ) (x : α) (l : List α) :
    upsert key x l = upsertBy (fun y => decide (key y = key x)) x l := by
  induction l with
  | nil => rfl
  | cons y ys ih => simp only [upsert, upsertBy, ih, decide_eq_true_eq]

/-- What an upsert loop started at `i` returns, when the prefix before `i`
has no match. -/
theorem upsert_loop_result {α} {κ : Type} [DecidableEq κ] (key : α → κ) (x : α) (v : List α) (i : Nat)
    (hi : i ≤ v.length) (hpre : upsert key x v = v.take i ++ upsert key x (v.drop i)) :
    searchFrom v (fun q => decide (key q = key x)) (fun j _ => v.set j x) (v ++ [x]) i =
      upsert key x v := by
  rw [searchFrom_upsert _ _ _ _ hi, ← upsert_eq, hpre]

/-- A delete loop keeps the rows satisfying `P`, pushing them into `out`. -/
theorem filter_split {α} (P : α → Bool) (l : List α) (k : Nat) :
    (l.take k).filter P ++ (l.drop k).filter P = l.filter P := by
  rw [← List.filter_append, List.take_append_drop]

theorem upsert_length {α} {κ : Type} [DecidableEq κ] (key : α → κ) (x : α) (l : List α) :
    (upsert key x l).length ≤ l.length + 1 := by
  induction l with
  | nil => simp [upsert]
  | cons y ys ih => rw [upsert]; split <;> simp; omega

section Lists
variable {α β : Type}

theorem mem_upsert_of {κ : Type} [DecidableEq κ] {k : α → κ} {x z : α} {l : List α}
    (h : z ∈ upsert k x l) : z = x ∨ z ∈ l := by
  induction l with
  | nil => simp [upsert] at h; exact Or.inl h
  | cons y ys ih =>
    unfold upsert at h
    split at h
    · simp at h; rcases h with h | h
      · exact Or.inl h
      · exact Or.inr (List.mem_cons_of_mem _ h)
    · simp at h; rcases h with h | h
      · exact Or.inr (h ▸ List.mem_cons_self)
      · rcases ih h with h | h
        · exact Or.inl h
        · exact Or.inr (List.mem_cons_of_mem _ h)

theorem mem_upsert_self {κ : Type} [DecidableEq κ] (k : α → κ) (x : α) (l : List α) : x ∈ upsert k x l := by
  induction l with
  | nil => simp [upsert]
  | cons y ys ih => unfold upsert; split <;> simp [ih]

theorem mem_upsert_of_ne {κ : Type} [DecidableEq κ] {k : α → κ} {x z : α} {l : List α}
    (hz : z ∈ l) (hk : k z ≠ k x) : z ∈ upsert k x l := by
  induction l with
  | nil => simp at hz
  | cons y ys ih =>
    unfold upsert
    simp at hz
    split
    · rename_i hy
      rcases hz with rfl | hz
      · exact absurd hy hk
      · exact List.mem_cons_of_mem _ hz
    · rcases hz with rfl | hz
      · exact List.mem_cons_self
      · exact List.mem_cons_of_mem _ (ih hz)

/-- With unique keys, `upsert` replaces exactly the row with `x`'s key. -/
theorem mem_upsert_iff {κ : Type} [DecidableEq κ] {k : α → κ} {x z : α} {l : List α} (hl : (l.map k).Nodup) :
    z ∈ upsert k x l ↔ z = x ∨ (z ∈ l ∧ k z ≠ k x) := by
  constructor
  · intro h
    induction l with
    | nil => simp [upsert] at h; exact Or.inl h
    | cons y ys ih =>
      simp only [List.map_cons, List.nodup_cons, List.mem_map] at hl
      unfold upsert at h
      split at h
      · rename_i hy
        simp at h; rcases h with h | h
        · exact Or.inl h
        · right; refine ⟨List.mem_cons_of_mem _ h, ?_⟩
          intro hzk; exact hl.1 ⟨z, h, hzk.trans hy.symm⟩
      · rename_i hy
        simp at h; rcases h with rfl | h
        · exact Or.inr ⟨List.mem_cons_self, hy⟩
        · rcases ih hl.2 h with h | ⟨h1, h2⟩
          · exact Or.inl h
          · exact Or.inr ⟨List.mem_cons_of_mem _ h1, h2⟩
  · rintro (rfl | ⟨hz, hk⟩)
    · exact mem_upsert_self k _ l
    · exact mem_upsert_of_ne hz hk

theorem nodup_map_upsert {κ : Type} [DecidableEq κ] (k : α → κ) (g : α → β) (hk : ∀ a b, k a = k b ↔ g a = g b)
    (x : α) (l : List α) (h : (l.map g).Nodup) : ((upsert k x l).map g).Nodup := by
  induction l with
  | nil => simp [upsert]
  | cons y ys ih =>
    simp only [List.map_cons, List.nodup_cons, List.mem_map] at h
    unfold upsert
    split
    · rename_i hy
      simp only [List.map_cons, List.nodup_cons, List.mem_map]
      refine ⟨?_, h.2⟩
      rintro ⟨z, hz, hzg⟩
      exact h.1 ⟨z, hz, hzg.trans ((hk y x).1 hy).symm⟩
    · rename_i hy
      simp only [List.map_cons, List.nodup_cons, List.mem_map]
      refine ⟨?_, ih h.2⟩
      rintro ⟨z, hz, hzg⟩
      rcases mem_upsert_of hz with rfl | hz
      · exact hy ((hk _ _).2 hzg.symm)
      · exact h.1 ⟨z, hz, hzg⟩

theorem nodup_map_k_of_g {κ : Type} [DecidableEq κ] (k : α → κ) (g : α → β) (hk : ∀ a b, k a = k b ↔ g a = g b)
    (l : List α) (h : (l.map g).Nodup) : (l.map k).Nodup := by
  induction l with
  | nil => simp
  | cons y ys ih =>
    simp only [List.map_cons, List.nodup_cons, List.mem_map] at h ⊢
    refine ⟨?_, ih h.2⟩
    rintro ⟨z, hz, hzk⟩
    exact h.1 ⟨z, hz, (hk _ _).1 hzk⟩

theorem nodup_map_filter (g : α → β) (p : α → Bool) (l : List α) (h : (l.map g).Nodup) :
    ((l.filter p).map g).Nodup :=
  (List.Sublist.map g List.filter_sublist).nodup h

/-- Keys are unique iff no element has a later element with the same key. -/
theorem nodup_map_iff_no_later {α β} (g : α → β) (l : List α) [DecidableEq β] :
    (l.map g).Nodup ↔ ∀ i (hi : i < l.length), ((l.drop (i + 1)).any (fun y => decide (g y = g l[i]))) = false := by
  rw [List.Nodup, List.pairwise_map, List.pairwise_iff_getElem]
  constructor
  · intro h i hi
    rw [Bool.eq_false_iff]
    intro hany
    simp only [List.any_eq_true, decide_eq_true_eq] at hany
    obtain ⟨y, hy, hgy⟩ := hany
    obtain ⟨j, hj, rfl⟩ := List.getElem_of_mem hy
    rw [List.getElem_drop] at hgy
    exact h i (i + 1 + j) hi (by simp at hj; omega) (by omega) hgy.symm
  · intro h i j hi hj hij hg
    have := h i hi
    rw [Bool.eq_false_iff] at this
    apply this
    simp only [List.any_eq_true, decide_eq_true_eq]
    refine ⟨l[j], ?_, hg.symm⟩
    have : j = (i + 1) + (j - (i + 1)) := by omega
    rw [List.mem_iff_getElem]
    exact ⟨j - (i + 1), by simp; omega, by rw [List.getElem_drop]; congr 1; omega⟩

end Lists

/-! ## More upsert facts -/

section Upsert
variable {α β κ : Type} [DecidableEq κ]

/-- A row with a new key is appended. -/
theorem upsert_fresh (k : α → κ) (x : α) (l : List α) (h : ∀ y ∈ l, k y ≠ k x) :
    upsert k x l = l ++ [x] := by
  induction l with
  | nil => rfl
  | cons y ys ih =>
    simp only [upsert, if_neg (h y List.mem_cons_self), List.cons_append]
    rw [ih (fun z hz => h z (List.mem_cons_of_mem _ hz))]

theorem mem_upsert_fresh {k : α → κ} {x z : α} {l : List α}
    (hf : ∀ y ∈ l, k y ≠ k x) : z ∈ upsert k x l ↔ z = x ∨ z ∈ l := by
  rw [upsert_fresh k x l hf, List.mem_append, List.mem_singleton, or_comm]

/-- Every key present before an upsert is present after. -/
theorem key_kept (k : α → κ) (x : α) {l : List α} {y : α} (hy : y ∈ l) :
    ∃ z ∈ upsert k x l, k z = k y := by
  by_cases h : k y = k x
  · exact ⟨x, mem_upsert_self k x l, h.symm⟩
  · exact ⟨y, mem_upsert_of_ne hy h, rfl⟩

theorem nodup_upsert (k : α → κ) (x : α) {l : List α} (h : (l.map k).Nodup) :
    ((upsert k x l).map k).Nodup :=
  nodup_map_upsert k k (fun _ _ => Iff.rfl) x l h

/-- Upserting by key keeps a second attribute unique, if no row with another
key has `x`'s value of it (usernames, emails, slugs). -/
theorem nodup_map_upsert_attr (k : α → κ) (g : α → β) (x : α) (l : List α)
    (hk : (l.map k).Nodup) (hg : (l.map g).Nodup) (hx : ∀ y ∈ l, g y = g x → k y = k x) :
    ((upsert k x l).map g).Nodup := by
  induction l with
  | nil => simp [upsert]
  | cons y ys ih =>
    simp only [List.map_cons, List.nodup_cons, List.mem_map] at hk hg
    unfold upsert
    split
    · rename_i hyx
      simp only [List.map_cons, List.nodup_cons, List.mem_map, not_exists, not_and]
      refine ⟨fun z hz hgz => hk.1 ⟨z, hz, ?_⟩, hg.2⟩
      rw [hx z (List.mem_cons_of_mem _ hz) hgz, hyx]
    · rename_i hyx
      simp only [List.map_cons, List.nodup_cons, List.mem_map, not_exists, not_and]
      refine ⟨fun z hz hgz => ?_, ih hk.2 hg.2 (fun z hz => hx z (List.mem_cons_of_mem _ hz))⟩
      rcases mem_upsert_of hz with rfl | hz
      · exact hyx (hx y List.mem_cons_self hgz.symm)
      · exact hg.1 ⟨z, hz, hgz⟩

/-- New sum plus the replaced row (or 0) is old sum plus the new row. -/
theorem sum_upsert {α κ} [DecidableEq κ] (key : α → κ) (f : α → Nat) (x : α) (l : List α) :
    ((upsert key x l).map f).sum + ((l.find? (fun y => key y = key x)).map f).getD 0 =
      (l.map f).sum + f x := by
  induction l with
  | nil => simp [upsert]
  | cons y ys ih =>
    by_cases h : key y = key x
    · simp [upsert, h]; omega
    · simp [upsert, h]; omega

/-- An upsert of a row outside the filter, whose key only rows outside the
filter have, leaves the filter unchanged. -/
theorem filter_upsert (k : α → κ) (p : α → Bool) (x : α) (l : List α)
    (hx : p x = false) (hk : ∀ y, k y = k x → p y = false) : (upsert k x l).filter p = l.filter p := by
  induction l with
  | nil => simp [upsert, hx]
  | cons y ys ih =>
    unfold upsert
    split
    · rename_i hy
      simp [hx, hk y hy]
    · simp only [List.filter_cons, ih]

/-- Counting after an upsert, when the count only depends on the key. -/
theorem length_filter_upsert (k : α → κ) (p : α → Bool) (x : α) (l : List α)
    (hp : ∀ y, k y = k x → p y = p x) :
    ((upsert k x l).filter p).length =
      (l.filter p).length + (if l.any (fun y => k y = k x) then 0 else if p x then 1 else 0) := by
  induction l with
  | nil => simp [upsert]; split <;> simp_all
  | cons y ys ih =>
    unfold upsert
    by_cases hy : k y = k x
    · simp [hy, hp y hy, List.filter_cons]; split <;> simp
    · simp only [hy, if_false, List.filter_cons, List.any_cons, decide_false, Bool.false_or]
      split <;> simp [ih]
      omega

/-- Counting after deleting a unique key. -/
theorem length_filter_remove (k : α → κ) (p : α → Bool) (kx : κ) (l : List α) (hl : (l.map k).Nodup)
    (hp : ∀ y, k y = kx → p y = true) :
    ((l.filter (fun y => ¬ k y = kx)).filter p).length =
      (l.filter p).length - (if l.any (fun y => k y = kx) then 1 else 0) := by
  induction l with
  | nil => simp
  | cons y ys ih =>
    simp only [List.map_cons, List.nodup_cons, List.mem_map] at hl
    by_cases hy : k y = kx
    · have hf : (ys.filter fun y => ¬ k y = kx) = ys := by
        rw [List.filter_eq_self]; intro z hz; simp only [decide_eq_true_eq]
        intro hkz; exact hl.1 ⟨z, hz, hkz.trans hy.symm⟩
      rw [List.filter_cons_of_neg (by simp [hy]), hf, List.filter_cons_of_pos (hp y hy)]
      simp [hy]
    · have hpos : ys.any (fun z => decide (k z = kx)) = true → 1 ≤ (ys.filter p).length := by
        intro hany; obtain ⟨z, hz, hkz⟩ := List.any_eq_true.1 hany
        exact List.length_pos_of_mem (List.mem_filter.2 ⟨hz, hp z (by simpa using hkz)⟩)
      have ih' := ih hl.2
      rw [List.filter_cons_of_pos (by simp [hy]), List.any_cons]
      simp only [hy, decide_false, Bool.false_or]
      by_cases hpy : p y = true
      · rw [List.filter_cons_of_pos hpy, List.filter_cons_of_pos hpy]
        simp only [List.length_cons, ih']
        split <;> simp_all
      · rw [List.filter_cons_of_neg hpy, List.filter_cons_of_neg hpy, ih']

end Upsert

end H5iAppLib
