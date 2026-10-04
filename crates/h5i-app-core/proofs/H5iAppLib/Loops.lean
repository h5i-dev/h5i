import Aeneas
/-!
# Specs for Aeneas loops over a vector

Aeneas turns `while i < v.len() { ... i += 1 }` into `loop body i`. `loop_search`
covers loops that stop at the first match, `loop_fold` loops that run to the
end. Each turns a per-step fact about `body` into a spec for the whole loop.
-/
open Aeneas Aeneas.Std Result

namespace H5iAppLib

/-- Scan `l` from index `k`: the first element satisfying `P` gives `found`,
reaching the end gives `missing`. -/
def searchFrom {α γ} (l : List α) (P : α → Bool) (found : Nat → α → γ) (missing : γ)
    (k : Nat) : γ :=
  if h : k < l.length then
    if P l[k] then found k l[k] else searchFrom l P found missing (k + 1)
  else missing
termination_by l.length - k

theorem searchFrom_found {α γ} {l : List α} {P : α → Bool} {found : Nat → α → γ} {missing : γ}
    {k : Nat} (h : k < l.length) (hp : P l[k]) : searchFrom l P found missing k = found k l[k] := by
  rw [searchFrom]; simp [h, hp]

theorem searchFrom_skip {α γ} {l : List α} {P : α → Bool} {found : Nat → α → γ} {missing : γ}
    {k : Nat} (h : k < l.length) (hp : ¬ P l[k]) :
    searchFrom l P found missing k = searchFrom l P found missing (k + 1) := by
  rw [searchFrom]; simp [h, hp]

theorem searchFrom_end {α γ} {l : List α} {P : α → Bool} {found : Nat → α → γ} {missing : γ}
    {k : Nat} (h : l.length ≤ k) : searchFrom l P found missing k = missing := by
  rw [searchFrom]; simp; omega

/-- Per-step shape of a search loop body at index `i`; `abs` reads the
result as a model value. -/
def SearchStep {α γ δ} (l : List α) (P : α → Bool) (abs : γ → δ) (found : Nat → α → δ)
    (missing : δ) (i : Usize) : ControlFlow Usize γ → Prop
  | .done y => (∃ h : i.val < l.length, P l[i.val] ∧ abs y = found i.val l[i.val]) ∨
      (l.length ≤ i.val ∧ abs y = missing)
  | .cont j => ∃ h : i.val < l.length, ¬ P l[i.val] ∧ j.val = i.val + 1

theorem loop_search {α γ δ : Type} (l : List α) (P : α → Bool) (abs : γ → δ)
    (found : Nat → α → δ) (missing : δ)
    (body : Usize → Result (ControlFlow Usize γ))
    (hstep : ∀ i : Usize, i.val ≤ l.length → body i ⦃ SearchStep l P abs found missing i ⦄)
    (i : Usize) (hi : i.val ≤ l.length) :
    loop body i ⦃ y => abs y = searchFrom l P found missing i.val ⦄ := by
  apply loop.spec_decr_nat (measure := fun (j : Usize) => l.length - j.val)
    (inv := fun j => j.val ≤ l.length ∧
      searchFrom l P found missing i.val = searchFrom l P found missing j.val)
  · rintro j ⟨hj, heq⟩
    apply WP.spec_mono (hstep j hj)
    intro r h
    cases r with
    | done y =>
      rcases h with ⟨hlt, hp, hy⟩ | ⟨hle, hy⟩
      · simp only; rw [hy, heq, searchFrom_found hlt hp]
      · simp only; rw [hy, heq, searchFrom_end hle]
    | cont k =>
      obtain ⟨hlt, hp, hk⟩ := h
      refine ⟨⟨by omega, ?_⟩, by omega⟩
      rw [heq, searchFrom_skip hlt hp, hk]
  · exact ⟨hi, rfl⟩

/-- Per-step shape of a fold loop body at state `(s, i)`: `abs` reads the
accumulator as a model value, `g` is the model step, `Inv` carries bounds. -/
def FoldStep {α σ β} (l : List α) (abs : σ → β) (g : β → α → β) (Inv : σ → Nat → Prop)
    (s : σ) (i : Usize) : ControlFlow (σ × Usize) σ → Prop
  | .done s' => l.length ≤ i.val ∧ abs s' = abs s
  | .cont (s', j) => ∃ h : i.val < l.length,
      abs s' = g (abs s) l[i.val] ∧ j.val = i.val + 1 ∧ Inv s' j.val

theorem loop_fold {α σ β : Type} (l : List α) (abs : σ → β) (g : β → α → β)
    (Inv : σ → Nat → Prop)
    (body : σ × Usize → Result (ControlFlow (σ × Usize) σ))
    (hstep : ∀ (s : σ) (i : Usize), i.val ≤ l.length → Inv s i.val →
      body (s, i) ⦃ FoldStep l abs g Inv s i ⦄)
    (s : σ) (i : Usize) (hi : i.val ≤ l.length) (hinv : Inv s i.val) :
    loop body (s, i) ⦃ s' => abs s' = (l.drop i.val).foldl g (abs s) ⦄ := by
  apply loop.spec_decr_nat (measure := fun (x : σ × Usize) => l.length - x.2.val)
    (inv := fun x => x.2.val ≤ l.length ∧ Inv x.1 x.2.val ∧
      (l.drop i.val).foldl g (abs s) = (l.drop x.2.val).foldl g (abs x.1))
  · rintro ⟨t, j⟩ ⟨hj, hI, heq⟩
    simp only at hj hI heq
    apply WP.spec_mono (hstep t j hj hI)
    intro r h
    cases r with
    | done t' =>
      obtain ⟨hle, habs⟩ := h
      simp only
      rw [heq, List.drop_eq_nil_of_le hle, habs]; rfl
    | cont x =>
      obtain ⟨t', k⟩ := x
      obtain ⟨hlt, habs, hk, hI'⟩ := h
      simp only
      refine ⟨⟨by omega, hI', ?_⟩, by omega⟩
      rw [heq, List.drop_eq_getElem_cons hlt, List.foldl_cons, ← habs, hk]
  · exact ⟨hi, hinv, rfl⟩

/-- Per-step shape of a fold loop with two accumulators, whose state Aeneas
lays out as `(a, b, i)`. -/
def FoldStep2 {α σ τ β} (l : List α) (abs : σ → τ → β) (g : β → α → β) (Inv : σ → τ → Nat → Prop)
    (a : σ) (b : τ) (i : Usize) : ControlFlow (σ × τ × Usize) (σ × τ) → Prop
  | .done (a', b') => l.length ≤ i.val ∧ a' = a ∧ b' = b
  | .cont (a', b', j) => ∃ h : i.val < l.length,
      abs a' b' = g (abs a b) l[i.val] ∧ j.val = i.val + 1 ∧ Inv a' b' j.val

theorem loop_fold2 {α σ τ β : Type} (l : List α) (abs : σ → τ → β) (g : β → α → β)
    (Inv : σ → τ → Nat → Prop)
    (body : σ × τ × Usize → Result (ControlFlow (σ × τ × Usize) (σ × τ)))
    (hstep : ∀ (a : σ) (b : τ) (i : Usize), i.val ≤ l.length → Inv a b i.val →
      body (a, b, i) ⦃ FoldStep2 l abs g Inv a b i ⦄)
    (a : σ) (b : τ) (i : Usize) (hi : i.val ≤ l.length) (hinv : Inv a b i.val) :
    loop body (a, b, i) ⦃ r => abs r.1 r.2 = (l.drop i.val).foldl g (abs a b) ∧ Inv r.1 r.2 l.length ⦄ := by
  apply loop.spec_decr_nat (measure := fun (x : σ × τ × Usize) => l.length - x.2.2.val)
    (inv := fun x => x.2.2.val ≤ l.length ∧ Inv x.1 x.2.1 x.2.2.val ∧
      (l.drop i.val).foldl g (abs a b) = (l.drop x.2.2.val).foldl g (abs x.1 x.2.1))
  · rintro ⟨t, u, j⟩ ⟨hj, hI, heq⟩
    simp only at hj hI heq
    apply WP.spec_mono (hstep t u j hj hI)
    intro r h
    cases r with
    | done y =>
      obtain ⟨t', u'⟩ := y
      obtain ⟨hle, rfl, rfl⟩ := h
      simp only
      have : j.val = l.length := by omega
      refine ⟨?_, this ▸ hI⟩
      rw [heq, List.drop_eq_nil_of_le hle]; rfl
    | cont x =>
      obtain ⟨t', u', k⟩ := x
      obtain ⟨hlt, habs, hk, hI'⟩ := h
      simp only
      refine ⟨⟨by omega, hI', ?_⟩, by omega⟩
      rw [heq, List.drop_eq_getElem_cons hlt, List.foldl_cons, ← habs, hk]
  · exact ⟨hi, hinv, rfl⟩

/-! ## Model functions the loops compute -/

theorem searchFrom_find {α β} (l : List α) (P : α → Bool) (f : α → β) (k : Nat) :
    searchFrom l P (fun _ x => some (f x)) none k = ((l.drop k).find? P).map f := by
  induction h : l.length - k generalizing k with
  | zero => rw [searchFrom_end (by omega), List.drop_eq_nil_of_le (by omega)]; rfl
  | succ n ih =>
    have hk : k < l.length := by omega
    rw [List.drop_eq_getElem_cons hk, List.find?_cons]
    by_cases hp : P l[k]
    · rw [searchFrom_found hk hp]; simp [hp]
    · rw [searchFrom_skip hk hp, ih (k + 1) (by omega)]; simp [hp]

/-- A lookup with a default: the first match's `f`, or `d`. -/
theorem searchFrom_findD {α β} (l : List α) (P : α → Bool) (f : α → β) (d : β) (k : Nat) :
    searchFrom l P (fun _ x => f x) d k = (((l.drop k).find? P).map f).getD d := by
  induction h : l.length - k generalizing k with
  | zero => rw [searchFrom_end (by omega), List.drop_eq_nil_of_le (by omega)]; rfl
  | succ n ih =>
    have hk : k < l.length := by omega
    rw [List.drop_eq_getElem_cons hk, List.find?_cons]
    by_cases hp : P l[k]
    · rw [searchFrom_found hk hp]; simp [hp]
    · rw [searchFrom_skip hk hp, ih (k + 1) (by omega)]; simp [hp]

/-- A search that returns a constant: `found` if any element from `k` on
satisfies `P`, else `missing`. Covers "all" checks (`false`/`true`) and
"exists" checks (`true`/`false`). -/
theorem searchFrom_const {α γ} (l : List α) (P : α → Bool) (b c : γ) (k : Nat) :
    searchFrom l P (fun _ _ => b) c k = if (l.drop k).any P then b else c := by
  induction h : l.length - k generalizing k with
  | zero => rw [searchFrom_end (by omega), List.drop_eq_nil_of_le (by omega)]; simp
  | succ n ih =>
    have hk : k < l.length := by omega
    rw [List.drop_eq_getElem_cons hk, List.any_cons]
    by_cases hp : P l[k]
    · rw [searchFrom_found hk hp]; simp [hp]
    · rw [searchFrom_skip hk hp, ih (k + 1) (by omega)]; simp [hp]

/-- Replace the first element satisfying `P` by `x`, or append `x`. -/
def upsertBy {α} (P : α → Bool) (x : α) : List α → List α
  | [] => [x]
  | y :: ys => if P y then x :: ys else y :: upsertBy P x ys

theorem searchFrom_upsert {α} (l : List α) (P : α → Bool) (x : α) (k : Nat) (hk : k ≤ l.length) :
    searchFrom l P (fun j _ => l.set j x) (l ++ [x]) k = l.take k ++ upsertBy P x (l.drop k) := by
  induction h : l.length - k generalizing k with
  | zero =>
    have : k = l.length := by omega
    subst this
    rw [searchFrom_end (le_refl _)]; simp [upsertBy]
  | succ n ih =>
    have hk' : k < l.length := by omega
    rw [List.drop_eq_getElem_cons hk']
    by_cases hp : P l[k]
    · rw [searchFrom_found hk' hp]; simp only [upsertBy, hp, if_true]
      rw [List.set_eq_take_append_cons_drop, if_pos hk']
    · rw [searchFrom_skip hk' hp, ih (k + 1) (by omega) (by omega)]
      simp only [upsertBy, hp, Bool.false_eq_true, if_false]
      rw [List.take_add_one, List.getElem?_eq_getElem hk', Option.toList_some, List.append_assoc]
      rfl

theorem foldl_count {α} (P : α → Bool) (l : List α) (c : Nat) :
    l.foldl (fun c x => c + if P x then 1 else 0) c = c + (l.filter P).length := by
  induction l generalizing c with
  | nil => simp
  | cons y ys ih => rw [List.foldl_cons, ih]; by_cases hp : P y <;> simp [hp]; omega

theorem foldl_filter {α} (P : α → Bool) (l acc : List α) :
    l.foldl (fun acc x => if P x then acc ++ [x] else acc) acc = acc ++ l.filter P := by
  induction l generalizing acc with
  | nil => simp
  | cons y ys ih => rw [List.foldl_cons, ih]; by_cases hp : P y <;> simp [hp]

/-- A search from 0 that returns constants: `b` if some element satisfies `P`, else `c`. -/
theorem search_bool {α γ} (l : List α) (P : α → Bool) (b c : γ) (r : γ)
    (hr : r = searchFrom l P (fun _ _ => b) c (↑(0#usize : Usize))) : r = if l.any P then b else c := by
  rw [hr, searchFrom_const, UScalar.ofNatCore_val_eq, List.drop_zero]

/-- An "exists" search from 0. -/
theorem search_any {α} (l : List α) (P : α → Bool) (r : Bool)
    (hr : r = searchFrom l P (fun _ _ => true) false (↑(0#usize : Usize))) : r = l.any P := by
  rw [search_bool _ _ _ _ _ hr]; cases l.any P <;> rfl

/-- A lookup from 0 that returns the first match. -/
theorem search_find {α} (l : List α) (P : α → Bool) (r : Option α)
    (hr : r = searchFrom l P (fun _ x => some x) none (↑(0#usize : Usize))) : r = l.find? P := by
  rw [hr, show (fun (_ : Nat) (x : α) => some x) = (fun _ x => some (_root_.id x)) from rfl,
    searchFrom_find, UScalar.ofNatCore_val_eq, List.drop_zero]; simp

/-- An "all" check: no element satisfies `P`. -/
theorem searchFrom_all {α} (l : List α) (P : α → Bool) :
    searchFrom l P (fun _ _ => false) true 0 = true ↔ ∀ x ∈ l, P x = false := by
  rw [searchFrom_const]; simp

/-- An "all" check from 0 that returns `true` when no element satisfies `P`. -/
theorem search_all {α} (l : List α) (P : α → Bool) (r : Bool)
    (hr : r = searchFrom l P (fun _ _ => false) true (↑(0#usize : Usize))) : r = l.all (fun x => !P x) := by
  rw [search_bool _ _ _ _ _ hr, List.all_eq_not_any_not]; simp only [Bool.not_not]
  cases l.any P <;> rfl

/-- A search that returns the index of the first match. -/
theorem searchFrom_findIdx {α} (l : List α) (P : α → Bool) (k : Nat) :
    searchFrom l P (fun i _ => some i) none k = ((l.drop k).findIdx? P).map (· + k) := by
  induction h : l.length - k generalizing k with
  | zero => rw [searchFrom_end (by omega), List.drop_eq_nil_of_le (by omega)]; rfl
  | succ n ih =>
    have hk : k < l.length := by omega
    rw [List.drop_eq_getElem_cons hk, List.findIdx?_cons]
    by_cases hp : P l[k]
    · rw [searchFrom_found hk hp]; simp [hp]
    · rw [searchFrom_skip hk hp, ih (k + 1) (by omega)]; simp [hp, Option.map_map]
      congr 1; funext x; omega

/-- A search that returns the index of the first match, or the length. -/
theorem searchFrom_index {α} (l : List α) (P : α → Bool) (k : Nat) :
    searchFrom l P (fun i _ => i) l.length k = min k l.length + ((l.drop k).takeWhile (fun x => !P x)).length := by
  induction h : l.length - k generalizing k with
  | zero => rw [searchFrom_end (by omega), List.drop_eq_nil_of_le (by omega)]; simp; omega
  | succ n ih =>
    have hk : k < l.length := by omega
    rw [List.drop_eq_getElem_cons hk, List.takeWhile_cons]
    by_cases hp : P l[k]
    · rw [searchFrom_found hk hp]; simp [hp]; omega
    · rw [searchFrom_skip hk hp, ih (k + 1) (by omega)]; simp [hp]; omega

/-- A search from 0 that returns the index of the first match. -/
theorem search_findIdx {α} (l : List α) (P : α → Bool) (r : Option Nat)
    (hr : r = searchFrom l P (fun i _ => some i) none (↑(0#usize : Usize))) : r = l.findIdx? P := by
  rw [hr, searchFrom_findIdx, UScalar.ofNatCore_val_eq, List.drop_zero]; simp

theorem foldl_map {α β} (f : α → β) (l : List α) (acc : List β) :
    l.foldl (fun acc x => acc ++ [f x]) acc = acc ++ l.map f := by
  induction l generalizing acc with
  | nil => simp
  | cons x xs ih => simp [ih]

theorem foldl_append_flatMap {α β} (f : α → List β) (l : List α) (acc : List β) :
    l.foldl (fun acc x => acc ++ f x) acc = acc ++ l.flatMap f := by
  induction l generalizing acc with
  | nil => simp
  | cons x xs ih => simp [ih]

theorem foldl_snoc {α} (l acc : List α) : l.foldl (fun acc x => acc ++ [x]) acc = acc ++ l := by
  induction l generalizing acc with
  | nil => simp
  | cons y ys ih => rw [List.foldl_cons, ih]; simp

/-- A loop that pushes `f x` for each `x` satisfying `P`. -/
theorem foldl_filter_map {α β} (P : α → Bool) (f : α → β) (l : List α) (acc : List β) :
    l.foldl (fun acc x => if P x then acc ++ [f x] else acc) acc = acc ++ (l.filter P).map f := by
  induction l generalizing acc with
  | nil => simp
  | cons x xs ih => by_cases h : P x <;> simp [h, ih]

end H5iAppLib
