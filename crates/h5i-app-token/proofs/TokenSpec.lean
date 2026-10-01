import Mathlib.Data.Nat.Digits.Lemmas
/-!
# Token bytes, specified over byte values

A token is `v1.<tenant>.<user>.<expiry>.<hex signature>`. The signature
covers the payload, which is everything before the last dot.
-/

namespace h5i_app_token.Spec

def u64max : Nat := 2 ^ 64 - 1

/-- Little-endian decimal digits, as `push_dec` produces them. -/
def digitsLE (n : Nat) : List Nat := n % 10 :: Nat.digits 10 (n / 10)

/-- ASCII decimal of `n`, no leading zeros. -/
def dec (n : Nat) : List Nat := ((digitsLE n).map (48 + ·)).reverse

def enc (t u e : Nat) : List Nat := [118, 49] ++ [46] ++ dec t ++ [46] ++ dec u ++ [46] ++ dec e

def hexd (d : Nat) : Nat := if d < 10 then 48 + d else 87 + d

def hexOf (b : Nat) : List Nat := [hexd (b / 16), hexd (b % 16)]

def joinS (p sig : List Nat) : List Nat := p ++ [46] ++ sig.flatMap hexOf

/-! ## Reading -/

def isDigit (c : Nat) : Bool := 48 ≤ c ∧ c ≤ 57

/-- Big-endian decimal value. -/
def fold (l : List Nat) : Nat := l.foldl (fun acc c => acc * 10 + (c - 48)) 0

/-- Strict decimal: nonempty, digits only, no leading zero, fits u64. -/
def decVal (l : List Nat) : Option Nat :=
  if l = [] then none
  else if l.head? = some 48 ∧ 1 < l.length then none
  else if l.all isDigit ∧ fold l ≤ u64max then some (fold l) else none

def hexv (c : Nat) : Nat :=
  if 48 ≤ c ∧ c ≤ 57 then c - 48 else if 97 ≤ c ∧ c ≤ 102 then c - 87 else 16

/-- Strict lowercase hex, even length. -/
def hexVal : List Nat → Option (List Nat)
  | [] => some []
  | [_] => none
  | a :: b :: r =>
    if hexv a < 16 ∧ hexv b < 16 then (hexVal r).map ((hexv a * 16 + hexv b) :: ·) else none

/-- Bytes before the first dot, and what follows that dot if there is one. -/
def splitDot : List Nat → List Nat × Option (List Nat)
  | [] => ([], none)
  | c :: r =>
    if c = 46 then ([], some r)
    else let (f, rest) := splitDot r; (c :: f, rest)

structure Tok where
  tenant : Nat
  user : Nat
  exp : Nat
  payload : List Nat
  sig : List Nat
deriving DecidableEq

def parseSpec (l : List Nat) : Option Tok :=
  match splitDot l with
  | (f0, some r0) =>
    match splitDot r0 with
    | (f1, some r1) =>
      match splitDot r1 with
      | (f2, some r2) =>
        match splitDot r2 with
        | (f3, some r3) =>
          match splitDot r3 with
          | (f4, none) =>
            if f0 = [118, 49] then
              match decVal f1, decVal f2, decVal f3, hexVal f4 with
              | some t, some u, some e, some sig =>
                some ⟨t, u, e, f0 ++ [46] ++ f1 ++ [46] ++ f2 ++ [46] ++ f3, sig⟩
              | _, _, _, _ => none
            else none
          | _ => none
        | _ => none
      | _ => none
    | _ => none
  | _ => none

/-! ## Facts about the pieces -/

theorem splitDot_cons (c : Nat) (l : List Nat) (hc : c ≠ 46) :
    splitDot (c :: l) = (c :: (splitDot l).1, (splitDot l).2) := by
  simp [splitDot, hc]

theorem splitDot_append {f r : List Nat} (hf : 46 ∉ f) :
    splitDot (f ++ [46] ++ r) = (f, some r) := by
  induction f with
  | nil => simp [splitDot]
  | cons c f ih =>
    simp only [List.mem_cons, not_or] at hf
    have h := ih hf.2
    simp only [List.cons_append]
    rw [splitDot_cons c _ (Ne.symm hf.1), h]

theorem splitDot_nodot {f : List Nat} (hf : 46 ∉ f) : splitDot f = (f, none) := by
  induction f with
  | nil => simp [splitDot]
  | cons c f ih =>
    simp only [List.mem_cons, not_or] at hf
    rw [splitDot_cons c _ (Ne.symm hf.1), ih hf.2]

theorem splitDot_some {l f r : List Nat} (h : splitDot l = (f, some r)) :
    46 ∉ f ∧ l = f ++ [46] ++ r := by
  induction l generalizing f with
  | nil => simp [splitDot] at h
  | cons c l ih =>
    by_cases hc : c = 46
    · simp only [splitDot, hc, if_true, Prod.mk.injEq, Option.some.injEq] at h
      obtain ⟨rfl, rfl⟩ := h
      simp [hc]
    · rw [splitDot_cons c l hc, Prod.mk.injEq] at h
      obtain ⟨rfl, h2⟩ := h
      obtain ⟨h3, h4⟩ := ih (f := (splitDot l).1) (Prod.ext rfl h2)
      exact ⟨by simp [h3, Ne.symm hc], by simp only [List.cons_append]; exact congrArg _ h4⟩

theorem splitDot_none {l f : List Nat} (h : splitDot l = (f, none)) : 46 ∉ f ∧ l = f := by
  induction l generalizing f with
  | nil =>
    simp only [splitDot, Prod.mk.injEq, and_true] at h
    subst h; simp
  | cons c l ih =>
    by_cases hc : c = 46
    · simp [splitDot, hc] at h
    · rw [splitDot_cons c l hc, Prod.mk.injEq] at h
      obtain ⟨rfl, h2⟩ := h
      obtain ⟨h3, h4⟩ := ih (f := (splitDot l).1) (Prod.ext rfl h2)
      exact ⟨by simp [h3, Ne.symm hc], congrArg _ h4⟩

theorem digitsLE_lt (n : Nat) : ∀ d ∈ digitsLE n, d < 10 := by
  intro d hd
  simp only [digitsLE, List.mem_cons] at hd
  rcases hd with rfl | hd
  · omega
  · exact Nat.digits_lt_base (by norm_num) hd

theorem dec_digits (n : Nat) : ∀ c ∈ dec n, 48 ≤ c ∧ c ≤ 57 := by
  intro c hc
  simp only [dec, List.mem_reverse, List.mem_map] at hc
  obtain ⟨d, hd, rfl⟩ := hc
  have := digitsLE_lt n d hd
  omega

theorem dec_nodot (n : Nat) : 46 ∉ dec n := fun h => by
  have := dec_digits n 46 h
  omega

theorem digitsLE_eq (n : Nat) : digitsLE n = if n = 0 then [0] else Nat.digits 10 n := by
  split
  · subst_vars; simp [digitsLE]
  · rename_i h
    rw [Nat.digits_def' (by norm_num) (by omega)]
    rfl

theorem fold_append (l : List Nat) (c : Nat) : fold (l ++ [c]) = fold l * 10 + (c - 48) := by
  simp [fold, List.foldl_append]

theorem fold_eq_ofDigits (l : List Nat) :
    fold l = Nat.ofDigits 10 (l.map (· - 48)).reverse := by
  induction l using List.reverseRecOn with
  | nil => simp [fold]
  | append_singleton l c ih =>
    rw [fold_append, ih]
    simp [Nat.ofDigits_cons]
    ring

theorem fold_dec (n : Nat) : fold (dec n) = n := by
  rw [fold_eq_ofDigits]
  have : ((dec n).map (· - 48)).reverse = digitsLE n := by
    simp [dec, List.map_reverse, List.map_map, Function.comp_def]
  rw [this, digitsLE_eq]
  split
  · subst_vars; simp
  · exact Nat.ofDigits_digits 10 n

theorem dec_head (n : Nat) : (dec n).head? = some 48 → n = 0 := by
  intro h
  by_contra hn
  rw [dec, digitsLE_eq, if_neg hn] at h
  have hne : Nat.digits 10 n ≠ [] := Nat.digits_ne_nil_iff_ne_zero.mpr hn
  have hl := Nat.getLast_digit_ne_zero 10 hn
  rw [List.head?_reverse, List.getLast?_map, List.getLast?_eq_getLast hne] at h
  simp at h
  omega

theorem dec_zero : dec 0 = [48] := by simp [dec, digitsLE]

theorem decVal_dec (n : Nat) (h : n ≤ u64max) : decVal (dec n) = some n := by
  have hne : dec n ≠ [] := by simp [dec, digitsLE]
  unfold decVal
  rw [if_neg hne]
  have hd : (dec n).all isDigit := by
    simp only [List.all_eq_true, isDigit, decide_eq_true_eq]
    exact dec_digits n
  rw [if_neg, if_pos ⟨hd, by rw [fold_dec]; exact h⟩, fold_dec]
  rintro ⟨hh, hl⟩
  have := dec_head n hh
  subst this
  simp [dec_zero] at hl

/-- A strict decimal has exactly one spelling. -/
theorem decVal_canon {l : List Nat} {n : Nat} (h : decVal l = some n) : l = dec n := by
  unfold decVal at h
  split at h; · simp at h
  rename_i hne
  split at h; · simp at h
  rename_i hlead
  split at h
  swap; · simp at h
  rename_i hok
  obtain ⟨hall, _⟩ := hok
  cases h
  have hdig : ∀ c ∈ l, 48 ≤ c ∧ c ≤ 57 := by
    simpa [List.all_eq_true, isDigit] using hall
  set D := (l.map (· - 48)).reverse with hD
  have hD10 : ∀ d ∈ D, d < 10 := by
    intro d hd
    simp only [hD, List.mem_reverse, List.mem_map] at hd
    obtain ⟨c, hc, rfl⟩ := hd
    have := hdig c hc
    omega
  have hl : l = (D.map (48 + ·)).reverse := by
    rw [hD, List.map_reverse, List.reverse_reverse, List.map_map]
    conv_lhs => rw [← List.map_id l]
    apply List.map_congr_left
    intro c hc
    have := hdig c hc
    simp only [Function.comp_apply, id]
    omega
  rw [fold_eq_ofDigits, ← hD]
  obtain ⟨c, l', rfl⟩ := List.exists_cons_of_ne_nil hne
  by_cases hc : c = 48 ∧ l' = []
  · obtain ⟨rfl, rfl⟩ := hc
    simp [hD, dec_zero]
  · have hc48 : c ≠ 48 := by
      intro e
      apply hc
      refine ⟨e, ?_⟩
      cases l' with
      | nil => rfl
      | cons x xs => exact absurd ⟨by simp [e], by simp⟩ hlead
    have hlast : ∀ (w : D ≠ []), D.getLast w ≠ 0 := by
      intro w
      simp only [hD, List.map_cons, List.reverse_cons, List.getLast_append_singleton]
      have := hdig c (by simp)
      omega
    have hdig' := Nat.digits_ofDigits 10 (by norm_num) D hD10 hlast
    have hn0 : Nat.ofDigits 10 D ≠ 0 := by
      intro h0
      rw [h0, Nat.digits_zero] at hdig'
      simp [hD] at hdig'
    rw [dec, digitsLE_eq, if_neg hn0, hdig']
    exact hl

theorem hexd_lt (d : Nat) (h : d < 16) : hexv (hexd d) = d := by
  unfold hexv hexd
  split_ifs <;> omega

theorem hexd_nodot (d : Nat) : hexd d ≠ 46 := by unfold hexd; split <;> omega

theorem hexVal_hex (sig : List Nat) (h : ∀ b ∈ sig, b < 256) :
    hexVal (sig.flatMap hexOf) = some sig := by
  induction sig with
  | nil => rfl
  | cons b sig ih =>
    have hb := h b (by simp)
    simp only [List.flatMap_cons, hexOf, List.cons_append, List.nil_append, hexVal]
    rw [hexd_lt _ (by omega), hexd_lt _ (by omega), if_pos (by omega),
      ih (fun x hx => h x (by simp [hx]))]
    simp only [Option.map_some]
    congr 2
    omega

theorem hex_nodot (sig : List Nat) : 46 ∉ sig.flatMap hexOf := by
  simp only [List.mem_flatMap, hexOf, List.mem_cons, List.not_mem_nil, or_false, not_exists, not_and]
  intro x _ h
  rcases h with h | h <;> exact hexd_nodot _ h.symm

theorem hexv_canon (c : Nat) (h : hexv c < 16) : hexd (hexv c) = c := by
  unfold hexv hexd at *
  split_ifs at * <;> omega

/-- Lowercase hex has exactly one spelling. -/
theorem hexVal_canon : ∀ {l sig : List Nat}, hexVal l = some sig →
    l = sig.flatMap hexOf ∧ ∀ b ∈ sig, b < 256
  | [], sig, h => by simp [hexVal] at h; subst h; simp
  | [_], _, h => by simp [hexVal] at h
  | a :: b :: r, sig, h => by
    unfold hexVal at h
    split at h
    · rename_i hab
      obtain ⟨sig', hr, rfl⟩ := Option.map_eq_some_iff.mp h
      obtain ⟨ih1, ih2⟩ := hexVal_canon hr
      refine ⟨?_, ?_⟩
      · subst ih1
        simp only [List.flatMap_cons, hexOf, List.cons_append, List.nil_append]
        have h1 : (hexv a * 16 + hexv b) / 16 = hexv a := by omega
        have h2 : (hexv a * 16 + hexv b) % 16 = hexv b := by omega
        rw [h1, h2, hexv_canon a hab.1, hexv_canon b hab.2]
      · intro x hx
        simp only [List.mem_cons] at hx
        rcases hx with rfl | hx
        · omega
        · exact ih2 x hx
    · simp at h

theorem enc_nodots (t u e : Nat) :
    splitDot (enc t u e) = ([118, 49], some (dec t ++ [46] ++ dec u ++ [46] ++ dec e)) := by
  have := splitDot_append (f := [118, 49]) (r := dec t ++ [46] ++ dec u ++ [46] ++ dec e) (by simp)
  simpa [enc, List.append_assoc] using this

/-! ## Main theorems -/

/-- Parsing an issued token gives back exactly what was signed. -/
theorem parse_join (t u e : Nat) (sig : List Nat)
    (ht : t ≤ u64max) (hu : u ≤ u64max) (he : e ≤ u64max) (hs : ∀ b ∈ sig, b < 256) :
    parseSpec (joinS (enc t u e) sig) = some ⟨t, u, e, enc t u e, sig⟩ := by
  have e1 : joinS (enc t u e) sig = [118, 49] ++ [46] ++
      (dec t ++ [46] ++ (dec u ++ [46] ++ (dec e ++ [46] ++ sig.flatMap hexOf))) := by
    simp [joinS, enc, List.append_assoc]
  unfold parseSpec
  simp only [e1, splitDot_append (by simp : 46 ∉ [118, 49]), splitDot_append (dec_nodot t),
    splitDot_append (dec_nodot u), splitDot_append (dec_nodot e), splitDot_nodot (hex_nodot sig),
    if_true, decVal_dec t ht, decVal_dec u hu, decVal_dec e he, hexVal_hex sig hs]
  simp [enc]

/-- An accepted token is the issued form of its fields; no other spelling parses. -/
theorem parse_canon {l : List Nat} {p : Tok} (h : parseSpec l = some p) :
    p.payload = enc p.tenant p.user p.exp ∧ l = joinS p.payload p.sig ∧
    p.tenant ≤ u64max ∧ p.user ≤ u64max ∧ p.exp ≤ u64max ∧ ∀ b ∈ p.sig, b < 256 := by
  unfold parseSpec at h
  split at h <;> try simp at h
  rename_i f0 r0 h0
  split at h <;> try simp at h
  rename_i f1 r1 h1
  split at h <;> try simp at h
  rename_i f2 r2 h2
  split at h <;> try simp at h
  rename_i f3 r3 h3
  split at h <;> try simp at h
  rename_i f4 h4
  split at h <;> try simp at h
  rename_i t u e sig ht hu he hs
  obtain ⟨hv, rfl⟩ := h
  obtain ⟨_, rfl⟩ := splitDot_some h0
  obtain ⟨_, rfl⟩ := splitDot_some h1
  obtain ⟨_, rfl⟩ := splitDot_some h2
  obtain ⟨_, rfl⟩ := splitDot_some h3
  obtain ⟨_, rfl⟩ := splitDot_none h4
  subst hv
  have bnd : ∀ {l n}, decVal l = some n → n ≤ u64max := by
    intro l n hn
    unfold decVal at hn
    split_ifs at hn with h1 h2 h3
    cases hn
    exact h3.2
  have bt := bnd ht
  have bu := bnd hu
  have be := bnd he
  have ct := decVal_canon ht
  have cu := decVal_canon hu
  have ce := decVal_canon he
  obtain ⟨cs, hsb⟩ := hexVal_canon hs
  subst ct cu ce cs
  exact ⟨by simp [enc], by simp [joinS], bt, bu, be, hsb⟩

/-- Different fields give different payloads, so a signature binds one reading. -/
theorem enc_injective {t u e t' u' e' : Nat}
    (ht : t ≤ u64max) (hu : u ≤ u64max) (he : e ≤ u64max)
    (ht' : t' ≤ u64max) (hu' : u' ≤ u64max) (he' : e' ≤ u64max)
    (h : enc t u e = enc t' u' e') : t = t' ∧ u = u' ∧ e = e' := by
  have h1 := parse_join t u e [] ht hu he (by simp)
  have h2 := parse_join t' u' e' [] ht' hu' he' (by simp)
  rw [h, h2] at h1
  simp only [Option.some.injEq, Tok.mk.injEq] at h1
  exact ⟨h1.1.symm, h1.2.1.symm, h1.2.2.1.symm⟩

end h5i_app_token.Spec
