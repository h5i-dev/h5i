import H5iAppLib.Basic
import Mathlib.Data.List.DropRight
/-!
# Byte strings, as lists

The models `h5i-app-std`'s `bytes` functions are specified by, and the facts
about them. `starts_with` is `<+:`, `ends_with` is `<:+`, `contains` is
`<:+:`, `split` is `List.splitOn`; the rest are defined here. Each spec in
`StdSpecs.lean` states a function's result as one of these, so a kernel
proof reasons about lists and never about the loops.
-/
open Aeneas Aeneas.Std Result

namespace H5iAppLib

/-- A byte-string literal: `lit "owner"`. Bytes of each `Char` mod 256, so the
UTF-8 bytes for ASCII text. `simp [lit]` evaluates it. -/
def lit (s : String) : List U8 :=
  s.toList.map (fun c => U8.ofNat (c.toNat % 256)
    (by have := Nat.mod_lt c.toNat (show 256 > 0 by decide); scalar_tac))

/-- `char::is_whitespace` on an ASCII byte. -/
def isWs (c : U8) : Bool := c.val = 32 || (9 ≤ c.val && c.val ≤ 13)

/-- `str::trim`, for ASCII whitespace. -/
def trimB (l : List U8) : List U8 := (l.dropWhile isWs).rdropWhile isWs

/-- `str::split` at byte `c`: `List.splitOn c`, with the predicate spelled
by `DecidableEq`. -/
def splitB (c : U8) (l : List U8) : List (List U8) := l.splitOnP (fun x => decide (x = c))

/-- `str::split_whitespace`, for ASCII whitespace. -/
def splitWs (l : List U8) : List (List U8) := (l.splitOnP isWs).filter (· ≠ [])

/-- `u8::to_ascii_lowercase`. -/
def lowerB (c : U8) : U8 :=
  if h : 65 ≤ c.val ∧ c.val ≤ 90 then U8.ofNat (c.val + 32) (by scalar_tac) else c

/-- A pattern that is exact or ends in `star`, which matches any rest. -/
def starMatch (star : U8) (p s : List U8) : Prop := p = s ∨ (p.getLast? = some star ∧ p.dropLast <+: s)

instance (star : U8) (p s : List U8) : Decidable (starMatch star p s) := by
  unfold starMatch; infer_instance

/-! ## Prefixes and suffixes -/

theorem prefix_iff_take {p s : List U8} : p <+: s ↔ p.length ≤ s.length ∧ s.take p.length = p := by
  constructor
  · intro h; exact ⟨h.length_le, List.prefix_iff_eq_take.1 h |>.symm⟩
  · rintro ⟨-, h⟩; rw [← h]; exact List.take_prefix _ _

theorem suffix_iff_drop {p s : List U8} : p <:+ s ↔ p.length ≤ s.length ∧ s.drop (s.length - p.length) = p := by
  constructor
  · intro h; exact ⟨h.length_le, List.suffix_iff_eq_drop.1 h |>.symm⟩
  · rintro ⟨-, h⟩; rw [← h]; exact List.drop_suffix _ _

theorem drop_takeWhile_length {α} (p : α → Bool) (l : List α) : l.drop (l.takeWhile p).length = l.dropWhile p := by
  induction l with
  | nil => rfl
  | cons x xs ih => by_cases h : p x <;> simp [h, ih]

/-- What `strip_prefix` returns. -/
theorem prefix_append_drop {p s : List U8} (h : p <+: s) : p ++ s.drop p.length = s := by
  obtain ⟨t, rfl⟩ := h; simp

/-- What `strip_suffix` returns. -/
theorem take_append_suffix {p s : List U8} (h : p <:+ s) : s.take (s.length - p.length) ++ p = s := by
  obtain ⟨t, rfl⟩ := h; simp

/-- A piece of `s` is a window of it. What `contains` checks. -/
theorem infix_iff_window {p s : List U8} :
    p <:+: s ↔ ∃ i, i + p.length ≤ s.length ∧ (s.drop i).take p.length = p := by
  constructor
  · rintro ⟨a, b, rfl⟩
    exact ⟨a.length, by simp, by simp⟩
  · rintro ⟨i, hi, h⟩
    refine ⟨s.take i, (s.drop i).drop p.length, ?_⟩
    conv => rhs; rw [← List.take_append_drop i s, ← List.take_append_drop p.length (s.drop i), h]
    simp

/-- `split_once` at byte `c`: what precedes its first occurrence, and what follows. -/
def splitOnce (c : U8) (l : List U8) : Option (List U8 × List U8) :=
  (l.findIdx? (fun x => decide (x = c))).map (fun i => (l.take i, l.drop (i + 1)))

theorem splitOnce_eq_some {c : U8} {l a b : List U8} (h : splitOnce c l = some (a, b)) :
    l = a ++ c :: b ∧ c ∉ a := by
  unfold splitOnce at h
  obtain ⟨i, hi, he⟩ := Option.map_eq_some_iff.1 h
  simp only [Prod.mk.injEq] at he; obtain ⟨rfl, rfl⟩ := he
  rw [List.findIdx?_eq_some_iff_getElem] at hi
  obtain ⟨hlt, hc, hpre⟩ := hi
  simp only [decide_eq_true_eq] at hc
  refine ⟨?_, ?_⟩
  · conv => lhs; rw [← List.take_append_drop i l]
    rw [List.drop_eq_getElem_cons hlt, hc]
  · intro hm
    obtain ⟨j, hj, hje⟩ := List.getElem_of_mem hm
    simp only [List.length_take] at hj
    have := hpre j (by omega)
    simp only [List.getElem_take] at hje
    simp [hje] at this

theorem splitOnce_eq_none {c : U8} {l : List U8} : splitOnce c l = none ↔ c ∉ l := by
  unfold splitOnce
  simp only [Option.map_eq_none_iff, List.findIdx?_eq_none_iff]
  constructor
  · intro h hm; simpa using h c hm
  · intro h x hx; simp only [decide_eq_false_iff_not]; rintro rfl; exact h hx

/-- One step of the `split` loop: close the piece at `c`, else extend it. -/
def splitStep (c : U8) (st : List (List U8) × List U8) (x : U8) : List (List U8) × List U8 :=
  if x = c then (st.1 ++ [st.2], []) else (st.1, st.2 ++ [x])

theorem foldl_splitStep (c : U8) (l : List U8) (o : List (List U8)) (cu : List U8) :
    (l.foldl (splitStep c) (o, cu)).1 ++ [(l.foldl (splitStep c) (o, cu)).2] =
      o ++ List.splitOnPPrepend (fun x => decide (x = c)) l cu.reverse := by
  induction l generalizing o cu with
  | nil => simp
  | cons x xs ih =>
    rw [List.foldl_cons]
    by_cases h : x = c
    · rw [List.splitOnPPrepend_cons_pos (p := fun x => decide (x = c)) (a := x) (by simp [h])]
      simp only [splitStep, h, if_true]; rw [ih]; simp
    · simp only [splitStep, h, if_false]; rw [ih]
      rw [List.splitOnPPrepend_cons_neg (p := fun x => decide (x = c)) (by simp [h])]; simp

/-- One step of the `split_whitespace` loop: close a nonempty piece at whitespace. -/
def splitWsStep (st : List (List U8) × List U8) (x : U8) : List (List U8) × List U8 :=
  if isWs x then (if st.2 ≠ [] then (st.1 ++ [st.2], []) else st) else (st.1, st.2 ++ [x])

theorem foldl_splitWsStep (l : List U8) (o : List (List U8)) (cu : List U8) :
    (l.foldl splitWsStep (o, cu)).1 ++
        (if (l.foldl splitWsStep (o, cu)).2 ≠ [] then [(l.foldl splitWsStep (o, cu)).2] else []) =
      o ++ (List.splitOnPPrepend isWs l cu.reverse).filter (· ≠ []) := by
  induction l generalizing o cu with
  | nil => by_cases h : cu = [] <;> simp [h]
  | cons x xs ih =>
    rw [List.foldl_cons]
    by_cases hw : isWs x = true
    · rw [List.splitOnPPrepend_cons_pos hw]
      by_cases hc : cu = []
      · subst hc; simp only [splitWsStep, hw, if_true, ne_eq, not_true_eq_false, if_false]; rw [ih]; simp
      · simp only [splitWsStep, hw, if_true, ne_eq, hc, not_false_eq_true, if_true]; rw [ih]; simp [hc]
    · rw [List.splitOnPPrepend_cons_neg (by simpa using hw)]
      simp only [splitWsStep, hw, Bool.false_eq_true, if_false]; rw [ih]; simp

/-! ## Whitespace and case -/

theorem trimB_nil : trimB [] = [] := rfl

/-- `trim` returns a piece of its input. -/
theorem trimB_infix (l : List U8) : trimB l <:+: l := by
  unfold trimB List.rdropWhile
  have h1 : ((l.dropWhile isWs).reverse.dropWhile isWs).reverse <+: l.dropWhile isWs := by
    rw [← List.reverse_suffix, List.reverse_reverse]; exact List.dropWhile_suffix _
  exact (h1.isInfix).trans (List.dropWhile_suffix _).isInfix

theorem lowerB_val (c : U8) : (lowerB c).val = if 65 ≤ c.val ∧ c.val ≤ 90 then c.val + 32 else c.val := by
  unfold lowerB; split <;> simp_all

/-! ## Patterns -/

theorem starMatch_self (star : U8) (p : List U8) : starMatch star p p := Or.inl rfl

/-- `starMatch` as `star_match` computes it, for a nonempty pattern other than `s`. -/
theorem starMatch_iff_last {star : U8} {p s : List U8} (h0 : p.length ≠ 0) (hne : p ≠ s) :
    starMatch star p s ↔ p[p.length - 1]'(by omega) = star ∧
      p.length - 1 ≤ s.length ∧ s.take (p.length - 1) = p.take (p.length - 1) := by
  have hlast : p.getLast? = some (p[p.length - 1]'(by omega)) := by rw [List.getLast?_eq_getElem?]; simp
  unfold starMatch
  simp only [hne, false_or, hlast, Option.some.injEq, List.dropLast_eq_take, prefix_iff_take,
    List.length_take, Nat.min_eq_left (Nat.sub_le _ _)]

/-- A pattern without a trailing `star` matches only itself. -/
theorem starMatch_exact {star : U8} {p s : List U8} (hp : p.getLast? ≠ some star) :
    starMatch star p s ↔ p = s := by
  unfold starMatch; constructor
  · rintro (h | ⟨h, -⟩); exact h; exact absurd h hp
  · exact Or.inl

/-- `w ++ [star]` matches every extension of `w`. -/
theorem starMatch_wild {star : U8} {w s : List U8} : starMatch star (w ++ [star]) s ↔ w ++ [star] = s ∨ w <+: s := by
  unfold starMatch; simp

/-- Matching is transitive through a middle pattern, unless the first pattern
is the middle one with one more `star`. -/
theorem starMatch_trans {star : U8} {p q s : List U8} (h₁ : starMatch star p q) (h₂ : starMatch star q s)
    (hpq : p.dropLast ≠ q) : starMatch star p s := by
  rcases h₁ with rfl | ⟨hp, hpre⟩
  · exact h₂
  rcases h₂ with rfl | ⟨hq, hqs⟩
  · exact Or.inr ⟨hp, hpre⟩
  refine Or.inr ⟨hp, ?_⟩
  have hq' : q = q.dropLast ++ [star] := by
    rw [List.getLast?_eq_some_iff] at hq; obtain ⟨ys, hys⟩ := hq; subst hys; simp
  rw [hq', List.prefix_concat_iff] at hpre
  rcases hpre with h | h
  · exact absurd (h.trans hq'.symm) hpq
  · exact h.trans hqs

end H5iAppLib
