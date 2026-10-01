import Aeneas
/-! List facts that the kernel proofs share: lookups, sums, and filters. -/

namespace H5iAppLib

section
variable {α : Type _} {β : Type _}

theorem find?_mem {P : α → Bool} {l : List α} {x : α} (h : l.find? P = some x) : x ∈ l ∧ P x = true :=
  ⟨List.mem_of_find?_eq_some h, List.find?_some h⟩

/-! ## Sums -/

theorem le_sum {α} (f : α → Nat) {l : List α} {a : α} (ha : a ∈ l) : f a ≤ (l.map f).sum := by
  induction l with
  | nil => simp at ha
  | cons y ys ih =>
    simp only [List.map_cons, List.sum_cons]
    rcases List.mem_cons.1 ha with rfl | h
    · omega
    · have := ih h; omega

/-- Two different rows together are at most the sum. -/
theorem add_le_sum {α} (f : α → Nat) {l : List α} {a b : α} (ha : a ∈ l) (hb : b ∈ l) (hne : a ≠ b) :
    f a + f b ≤ (l.map f).sum := by
  induction l with
  | nil => simp at ha
  | cons y ys ih =>
    simp only [List.map_cons, List.sum_cons]
    rcases List.mem_cons.1 ha with rfl | ha' <;> rcases List.mem_cons.1 hb with rfl | hb'
    · exact absurd rfl hne
    · have := le_sum f hb'; omega
    · have := le_sum f ha'; omega
    · have := ih ha' hb'; omega

/-! ## Reading through a filter

If every row a reader looks at passes `q`, filtering by `q` first changes
nothing. These are the steps of a two-run (noninterference) proof. -/

theorem find?_filter_of_imp (l : List α) (p q : α → Bool) (h : ∀ x ∈ l, p x = true → q x = true) :
    (l.filter q).find? p = l.find? p := by
  induction l with
  | nil => rfl
  | cons x xs ih =>
    have ih' := ih (fun y hy => h y (List.mem_cons_of_mem _ hy))
    by_cases hq : q x = true
    · simp [hq, List.find?_cons, ih']
    · have hp : p x = false := by
        cases hpx : p x
        · rfl
        · exact absurd (h x List.mem_cons_self hpx) hq
      simp [hq, hp, ih']

theorem any_filter_of_imp (l : List α) (p q : α → Bool) (h : ∀ x ∈ l, p x = true → q x = true) :
    (l.filter q).any p = l.any p := by
  induction l with
  | nil => rfl
  | cons x xs ih =>
    have ih' := ih (fun y hy => h y (List.mem_cons_of_mem _ hy))
    by_cases hq : q x = true
    · simp [hq, ih']
    · have hp : p x = false := by
        cases hpx : p x
        · rfl
        · exact absurd (h x List.mem_cons_self hpx) hq
      simp [hq, hp, ih']

theorem filter_filter_of_imp (l : List α) (p q : α → Bool) (h : ∀ x ∈ l, p x = true → q x = true) :
    (l.filter q).filter p = l.filter p := by
  rw [List.filter_filter]
  apply List.filter_congr
  intro x hx
  cases hp : p x <;> simp [hp, h x hx]

/-- A fold that skips the rows failing `q` can run on the filtered list. -/
theorem foldl_filter_of_skip (l : List α) (g : β → α → β) (q : α → Bool) (b : β)
    (h : ∀ b x, q x = false → g b x = b) : (l.filter q).foldl g b = l.foldl g b := by
  induction l generalizing b with
  | nil => rfl
  | cons x xs ih =>
    by_cases hq : q x = true
    · simp [hq, ih]
    · simp [hq, h b x (by simpa using hq), ih]

end

end H5iAppLib
