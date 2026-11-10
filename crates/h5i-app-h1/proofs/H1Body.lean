import H1Spec
/-!
# Whole chunked bodies

`Spec.dechunk` reads a `chunked` body from a list of bytes, chunk by chunk,
without `chunkStep`. `feed` folds `chunkStep` over the bytes, as the server
does. `feed_dechunk` says they agree; `dechunk_local` says the end of the
body depends only on the bytes before it.
-/

namespace h5i_app_h1.Spec

/-- A whole-body result: the payload and the bytes consumed (framing
included), a rejection, or a need for more bytes. -/
inductive DR where
  | more
  | reject (status : Nat)
  | done (payload : List Nat) (consumed : Nat)
  deriving DecidableEq, Repr

/-- `payload` and `consumed` after `acc` and `n` bytes already seen. -/
def DR.shift (acc : List Nat) (n : Nat) : DR → DR
  | .done p k => .done (acc ++ p) (n + k)
  | r => r

def isHex (c : Nat) : Bool := decide (hexv c < 16)

/-- Hex digits `ds` after the value `s`. -/
def hexVal (s : Nat) (ds : List Nat) : Nat := ds.foldl (fun a c => a * 16 + hexv c) s

theorem dropWhile_length_le (p : Nat → Bool) (L : List Nat) : (L.dropWhile p).length ≤ L.length := by
  induction L with
  | nil => simp
  | cons x L ih => rw [List.dropWhile_cons]; split <;> simp <;> omega

/-- The chunks from the start of `L`, `total` body bytes so far:
`1*8HEXDIG CRLF` then that many bytes and CRLF, until a zero size, which is
followed by CRLF (no trailer fields). A size passing `mb` in total is 413. -/
def dechunkFrom (L : List Nat) (mb total : Nat) : DR :=
  let ds := L.takeWhile isHex
  if 8 < ds.length then .reject 400 else
  match h1 : L.dropWhile isHex with
  | [] => .more
  | c :: L2 =>
    if c ≠ 13 ∨ ds = [] then .reject 400 else
    if mb < total + hexVal 0 ds then .reject 413 else
    match L2 with
    | [] => .more
    | c2 :: L3 =>
      if c2 ≠ 10 then .reject 400 else
      if hexVal 0 ds = 0 then
        match L3 with
        | [] => .more
        | c3 :: L4 =>
          if c3 ≠ 13 then .reject 400 else
          match L4 with
          | [] => .more
          | c4 :: _ => if c4 ≠ 10 then .reject 400 else .done [] (ds.length + 4)
      else if L3.length < hexVal 0 ds then .more else
      match h2 : L3.drop (hexVal 0 ds) with
      | [] => .more
      | c3 :: L4 =>
        if c3 ≠ 13 then .reject 400 else
        match L4 with
        | [] => .more
        | c4 :: L5 =>
          if c4 ≠ 10 then .reject 400 else
          (dechunkFrom L5 mb (total + hexVal 0 ds)).shift (L3.take (hexVal 0 ds))
            (ds.length + 4 + hexVal 0 ds)
termination_by L.length
decreasing_by
  have a := congrArg List.length h1
  have b := congrArg List.length h2
  have c := dropWhile_length_le isHex L
  simp only [List.length_cons, List.length_drop] at a b
  omega

/-- A `chunked` body at the start of `L`, at most `mb` bytes of payload. -/
def dechunk (L : List Nat) (mb : Nat) : DR := dechunkFrom L mb 0

/-! ## Folding `chunkStep` -/

/-- A fold of `chunkStep`: `more` with the state and the payload so far when
the bytes run out. -/
inductive FR where
  | more (st : CState) (payload : List Nat)
  | reject (status : Nat)
  | done (payload : List Nat) (consumed : Nat)
  deriving DecidableEq, Repr

/-- `chunkStep` over `L` from `st`, with payload `acc` and `k` bytes seen. -/
def feed (st : CState) (L : List Nat) (mb : Nat) (acc : List Nat) (k : Nat) : FR :=
  match L with
  | [] => .more st acc
  | b :: L =>
    match chunkStep st b mb with
    | (s, .nothing) => feed s L mb acc (k + 1)
    | (s, .data) => feed s L mb (acc ++ [b]) (k + 1)
    | (_, .done) => .done acc (k + 1)
    | (_, .reject x) => .reject x

def FR.view : FR → DR
  | .more _ _ => .more
  | .reject s => .reject s
  | .done p k => .done p k

theorem feed_nil (st : CState) (mb : Nat) (acc : List Nat) (k : Nat) :
    feed st [] mb acc k = .more st acc := rfl

theorem feed_cons (st : CState) (b : Nat) (L : List Nat) (mb : Nat) (acc : List Nat) (k : Nat) :
    feed st (b :: L) mb acc k = match chunkStep st b mb with
      | (s, .nothing) => feed s L mb acc (k + 1)
      | (s, .data) => feed s L mb (acc ++ [b]) (k + 1)
      | (_, .done) => .done acc (k + 1)
      | (_, .reject x) => .reject x := rfl

/-- Feeding `L₁ ++ L₂` is feeding `L₁`, then `L₂` from where it stopped. -/
theorem feed_append (st : CState) (L₁ L₂ : List Nat) (mb : Nat) (acc : List Nat) (k : Nat) :
    feed st (L₁ ++ L₂) mb acc k = match feed st L₁ mb acc k with
      | .more s a => feed s L₂ mb a (k + L₁.length)
      | r => r := by
  induction L₁ generalizing st acc k with
  | nil => simp [feed_nil]
  | cons b L ih =>
    simp only [List.cons_append, feed_cons, List.length_cons]
    have e : k + 1 + L.length = k + (L.length + 1) := by omega
    rcases h : chunkStep st b mb with ⟨s, o⟩
    cases o <;> simp only [ih, e]

/-- A `done` from `feed` read exactly `consumed - k` bytes: those bytes
followed by anything give the same result. -/
theorem feed_local (st : CState) (L : List Nat) (mb : Nat) (acc p : List Nat) (k n : Nat)
    (h : feed st L mb acc k = .done p n) :
    k < n ∧ n - k ≤ L.length ∧ ∀ R, feed st (L.take (n - k) ++ R) mb acc k = .done p n := by
  induction L generalizing st acc k with
  | nil => simp [feed_nil] at h
  | cons b L ih =>
    rw [feed_cons] at h
    rcases hs : chunkStep st b mb with ⟨s, o⟩
    rw [hs] at h
    cases o with
    | nothing =>
      obtain ⟨h1, h2, h3⟩ := ih s acc (k + 1) h
      refine ⟨by omega, by simp; omega, fun R => ?_⟩
      rw [show n - k = (n - (k + 1)) + 1 by omega, List.take_succ_cons, List.cons_append, feed_cons, hs]
      exact h3 R
    | data =>
      obtain ⟨h1, h2, h3⟩ := ih s (acc ++ [b]) (k + 1) h
      refine ⟨by omega, by simp; omega, fun R => ?_⟩
      rw [show n - k = (n - (k + 1)) + 1 by omega, List.take_succ_cons, List.cons_append, feed_cons, hs]
      exact h3 R
    | done =>
      simp only [FR.done.injEq] at h
      obtain ⟨rfl, rfl⟩ := h
      refine ⟨by omega, by simp, fun R => ?_⟩
      rw [show k + 1 - k = 0 + 1 by omega, List.take_succ_cons, List.cons_append, feed_cons, hs]
    | reject x => simp at h

/-! ## The fold computes `dechunk` -/

theorem shift_shift (a b : List Nat) (m n : Nat) (r : DR) :
    (r.shift b n).shift a m = r.shift (a ++ b) (m + n) := by
  cases r <;> simp [DR.shift, List.append_assoc, Nat.add_assoc]

theorem shift_nil (r : DR) : r.shift [] 0 = r := by cases r <;> simp [DR.shift]

theorem hexVal_cons (s c : Nat) (ds : List Nat) : hexVal s (c :: ds) = hexVal (s * 16 + hexv c) ds := rfl

theorem hexVal_lt (s d : Nat) (ds : List Nat) (hall : ∀ c ∈ ds, isHex c = true) (hs : s < 16 ^ d) :
    hexVal s ds < 16 ^ (d + ds.length) := by
  induction ds generalizing s d with
  | nil => simpa [hexVal] using hs
  | cons c ds ih =>
    rw [hexVal_cons, List.length_cons, show d + (ds.length + 1) = (d + 1) + ds.length by omega]
    apply ih _ _ (fun x hx => hall x (List.mem_cons_of_mem _ hx))
    have hc : hexv c < 16 := by simpa [isHex] using hall c List.mem_cons_self
    rw [Nat.pow_succ]; omega

/-! One lemma per phase of `chunkStep`. -/

theorem cs0_hex {s d t b mb : Nat} (h : hexv b < 16) (hd : d < 8) (hs : s ≤ 268435455) :
    chunkStep ⟨0, s, d, t⟩ b mb = (⟨0, s * 16 + hexv b, d + 1, t⟩, .nothing) := by
  simp [chunkStep, sizeStep, h]; omega
theorem cs0_ninth {s t b mb : Nat} (h : hexv b < 16) : (chunkStep ⟨0, s, 8, t⟩ b mb).2 = .reject 400 := by
  simp [chunkStep, sizeStep, h]
theorem cs0_bad {s d t b mb : Nat} (h : 16 ≤ hexv b) (hb : b ≠ 13 ∨ d = 0) :
    (chunkStep ⟨0, s, d, t⟩ b mb).2 = .reject 400 := by
  rcases hb with hb | hb <;> simp [chunkStep, sizeStep, show ¬ hexv b < 16 by omega, hb]
theorem cs0_big {s d t mb : Nat} (hd : 0 < d) (h : mb < t ∨ mb - t < s) :
    (chunkStep ⟨0, s, d, t⟩ 13 mb).2 = .reject 413 := by
  simp [chunkStep, sizeStep, hexv, hd, h]
theorem cs0_cr {s d t mb : Nat} (hd : 0 < d) (h : ¬ (mb < t ∨ mb - t < s)) :
    chunkStep ⟨0, s, d, t⟩ 13 mb = (⟨1, s, d, t + s⟩, .nothing) := by
  simp only [chunkStep, sizeStep, hexv]; simp [hd]; omega
theorem cs1_bad {s d t b mb : Nat} (h : b ≠ 10) : (chunkStep ⟨1, s, d, t⟩ b mb).2 = .reject 400 := by
  simp [chunkStep, h]
theorem cs1_lf {s d t mb : Nat} :
    chunkStep ⟨1, s, d, t⟩ 10 mb = (⟨if s = 0 then 5 else 2, s, 0, t⟩, .nothing) := by
  simp [chunkStep]
theorem cs2 {s d t b mb : Nat} :
    chunkStep ⟨2, s, d, t⟩ b mb = if s ≤ 1 then (⟨3, 0, d, t⟩, .data) else (⟨2, s - 1, d, t⟩, .data) := by
  simp [chunkStep]
theorem cs3_bad {s d t b mb : Nat} (h : b ≠ 13) : (chunkStep ⟨3, s, d, t⟩ b mb).2 = .reject 400 := by
  simp [chunkStep, expectStep, h]
theorem cs3 {s d t mb : Nat} : chunkStep ⟨3, s, d, t⟩ 13 mb = (⟨4, s, d, t⟩, .nothing) := by
  simp [chunkStep, expectStep]
theorem cs4_bad {s d t b mb : Nat} (h : b ≠ 10) : (chunkStep ⟨4, s, d, t⟩ b mb).2 = .reject 400 := by
  simp [chunkStep, expectStep, h]
theorem cs4 {s d t mb : Nat} : chunkStep ⟨4, s, d, t⟩ 10 mb = (⟨0, 0, d, t⟩, .nothing) := by
  simp [chunkStep, expectStep]
theorem cs5_bad {s d t b mb : Nat} (h : b ≠ 13) : (chunkStep ⟨5, s, d, t⟩ b mb).2 = .reject 400 := by
  simp [chunkStep, expectStep, h]
theorem cs5 {s d t mb : Nat} : chunkStep ⟨5, s, d, t⟩ 13 mb = (⟨6, s, d, t⟩, .nothing) := by
  simp [chunkStep, expectStep]
theorem cs6_bad {s d t b mb : Nat} (h : b ≠ 10) : (chunkStep ⟨6, s, d, t⟩ b mb).2 = .reject 400 := by
  simp [chunkStep, expectStep, h]
theorem cs6 {s d t mb : Nat} : (chunkStep ⟨6, s, d, t⟩ 10 mb).2 = .done := by
  simp [chunkStep, expectStep]

/-- A byte whose step rejects ends the fold there. -/
theorem feed_reject {st : CState} {b : Nat} {L : List Nat} {mb : Nat} {acc : List Nat} {k x : Nat}
    (h : (chunkStep st b mb).2 = .reject x) : feed st (b :: L) mb acc k = .reject x := by
  rw [feed_cons]; rcases hs : chunkStep st b mb with ⟨s, o⟩; rw [hs] at h; simp only at h; subst h; rfl

theorem feed_nothing {st s : CState} {b : Nat} {L : List Nat} {mb : Nat} {acc : List Nat} {k : Nat}
    (h : chunkStep st b mb = (s, .nothing)) : feed st (b :: L) mb acc k = feed s L mb acc (k + 1) := by
  rw [feed_cons, h]

theorem feed_done {st : CState} {b : Nat} {L : List Nat} {mb : Nat} {acc : List Nat} {k : Nat}
    (h : (chunkStep st b mb).2 = .done) : feed st (b :: L) mb acc k = .done acc (k + 1) := by
  rw [feed_cons]; rcases hs : chunkStep st b mb with ⟨s, o⟩; rw [hs] at h; simp only at h; subst h; rfl

/-- Size digits, while there are at most eight. -/
theorem feed_digits (ds R : List Nat) (mb s d t : Nat) (acc : List Nat) (k : Nat)
    (hall : ∀ c ∈ ds, isHex c = true) (hlen : d + ds.length ≤ 8) (hs : s < 16 ^ d) :
    feed ⟨0, s, d, t⟩ (ds ++ R) mb acc k = feed ⟨0, hexVal s ds, d + ds.length, t⟩ R mb acc (k + ds.length) := by
  induction ds generalizing s d k with
  | nil => simp [hexVal]
  | cons c ds ih =>
    have hc : hexv c < 16 := by simpa [isHex] using hall c List.mem_cons_self
    have hd : d < 8 := by simp at hlen; omega
    have h7 : 16 ^ d ≤ 16 ^ 7 := Nat.pow_le_pow_right (by omega) (by omega)
    have h7' : (16 : Nat) ^ 7 = 268435456 := by rfl
    rw [List.cons_append, feed_nothing (cs0_hex hc hd (by omega)),
      ih (s * 16 + hexv c) (d + 1) (k + 1) (fun x hx => hall x (List.mem_cons_of_mem _ hx))
        (by simp at hlen; omega) (by rw [Nat.pow_succ]; omega)]
    simp only [hexVal_cons, List.length_cons]
    rw [show d + 1 + ds.length = d + (ds.length + 1) by omega, show k + 1 + ds.length = k + (ds.length + 1) by omega]

/-- A ninth size digit is 400. -/
theorem feed_too_many (ds R : List Nat) (mb t : Nat) (acc : List Nat) (k : Nat)
    (hall : ∀ c ∈ ds, isHex c = true) (hlen : 8 < ds.length) :
    feed ⟨0, 0, 0, t⟩ (ds ++ R) mb acc k = .reject 400 := by
  have hsplit : ds = ds.take 8 ++ (ds[8]'hlen :: ds.drop 9) := by
    rw [← List.drop_eq_getElem_cons hlen, List.take_append_drop]
  have h8 : (ds.take 8).length = 8 := by simp; omega
  have hall8 : ∀ c ∈ ds.take 8, isHex c = true := fun c hc => hall c (List.mem_of_mem_take hc)
  rw [hsplit, List.append_assoc, feed_digits _ _ mb 0 0 t acc k hall8 (by omega) (by simp), h8,
    List.cons_append]
  have hc : hexv (ds[8]'hlen) < 16 := by simpa [isHex] using hall _ (List.getElem_mem hlen)
  exact feed_reject (cs0_ninth hc)

/-- Chunk data: exactly `size` bytes, whatever they are. -/
theorem feed_data (D R : List Nat) (mb t : Nat) (acc : List Nat) (k : Nat) (hD : D ≠ []) :
    feed ⟨2, D.length, 0, t⟩ (D ++ R) mb acc k = feed ⟨3, 0, 0, t⟩ R mb (acc ++ D) (k + D.length) := by
  induction D generalizing acc k with
  | nil => exact absurd rfl hD
  | cons b D ih =>
    rw [List.cons_append, feed_cons, cs2]
    by_cases h0 : D = []
    · subst h0
      simp
    · have hl : D.length ≠ 0 := by simpa using h0
      rw [if_neg (by simp; omega)]
      simp only [List.length_cons, Nat.add_sub_cancel]
      rw [ih (acc ++ [b]) (k + 1) h0]
      simp only [List.append_assoc, List.singleton_append]
      congr 1; omega

theorem feed_data' (D R : List Nat) (n mb t : Nat) (acc : List Nat) (k : Nat) (hD : D ≠ []) (hn : D.length = n) :
    feed ⟨2, n, 0, t⟩ (D ++ R) mb acc k = feed ⟨3, 0, 0, t⟩ R mb (acc ++ D) (k + n) := by
  subst hn; exact feed_data D R mb t acc k hD

/-- Fewer than `size` data bytes: more needed. -/
theorem feed_data_short (L : List Nat) (n mb t : Nat) (acc : List Nat) (k : Nat) (hn : L.length < n) :
    ∃ s a, feed ⟨2, n, 0, t⟩ L mb acc k = .more s a := by
  induction L generalizing n acc k with
  | nil => exact ⟨_, _, feed_nil _ _ _ _⟩
  | cons b L ih =>
    simp only [List.length_cons] at hn
    rw [feed_cons, cs2, if_neg (by omega)]
    exact ih (n - 1) _ _ (by omega)

theorem takeWhile_all (p : Nat → Bool) (L : List Nat) : ∀ c ∈ L.takeWhile p, p c = true := by
  induction L with
  | nil => simp
  | cons x L ih =>
    rw [List.takeWhile_cons]
    split
    · rename_i hx; intro c hc; simp only [List.mem_cons] at hc; rcases hc with rfl | hc; exact hx; exact ih c hc
    · simp

theorem dropWhile_head {p : Nat → Bool} {L L2 : List Nat} {c : Nat} (h : L.dropWhile p = c :: L2) :
    p c = false := by
  induction L with
  | nil => simp at h
  | cons x L ih =>
    rw [List.dropWhile_cons] at h
    split at h
    · exact ih h
    · rename_i hx; simp at h; rw [← h.1]; simpa using hx

theorem feed_view_shift_more {st : CState} {L : List Nat} {mb : Nat} {acc : List Nat} {k : Nat}
    (h : ∃ s a, feed st L mb acc k = .more s a) : (feed st L mb acc k).view = DR.more.shift acc k := by
  obtain ⟨s, a, h⟩ := h; rw [h]; rfl

/-- The fold from the start of a chunk computes `dechunkFrom`. -/
theorem feed_chunks (L : List Nat) (mb t : Nat) (acc : List Nat) (k : Nat) (ht : t ≤ mb) :
    (feed ⟨0, 0, 0, t⟩ L mb acc k).view = (dechunkFrom L mb t).shift acc k := by
  induction hL : L.length using Nat.strongRecOn generalizing L t acc k with
  | _ len ih =>
  subst hL
  have hsplit := (List.takeWhile_append_dropWhile (p := isHex) (l := L)).symm
  have hall : ∀ c ∈ L.takeWhile isHex, isHex c = true := takeWhile_all isHex L
  rw [dechunkFrom]
  try simp only
  conv => lhs; rw [hsplit]
  clear hsplit
  split
  · rename_i h9
    rw [feed_too_many _ _ mb t acc k hall h9]; rfl
  rename_i h8
  have h8' : 0 + (L.takeWhile isHex).length ≤ 8 := by omega
  split
  · rename_i hd
    conv => lhs; rw [hd, List.append_nil, ← List.append_nil (L.takeWhile isHex)]
    rw [feed_digits _ [] mb 0 0 t acc k hall h8' (by simp)]; rfl
  rename_i c L2 hd
  have hc : isHex c = false := dropWhile_head hd
  have hcv : 16 ≤ hexv c := by simpa [isHex] using hc
  conv => lhs; rw [hd]
  rw [feed_digits _ _ mb 0 0 t acc k hall h8' (by simp)]
  have hlen5 : ∀ L5 : List Nat, L.dropWhile isHex = c :: L2 → L5.length + 2 + (hexVal 0 (L.takeWhile isHex)) ≤ L2.length →
      L5.length < L.length := by
    intro L5 h _
    have a := congrArg List.length h
    have c := dropWhile_length_le isHex L
    simp only [List.length_cons] at a
    omega
  split
  · -- not CR, or no digits
    rename_i hbad
    rw [feed_reject (cs0_bad hcv (by rcases hbad with h | h; exact Or.inl h; exact Or.inr (by simp [h])))]; rfl
  rename_i hok
  simp only [not_or, Decidable.not_not] at hok
  obtain ⟨hcr, hne⟩ := hok
  subst hcr
  have hpos : 0 < 0 + (L.takeWhile isHex).length := by simpa using List.length_pos_iff.mpr hne
  split
  · rename_i hbig
    rw [feed_reject (cs0_big hpos (by omega))]; rfl
  rename_i hfit
  rw [feed_nothing (cs0_cr hpos (by omega))]
  split
  · rfl
  rename_i c2 L3 _
  split
  · rename_i hlf
    rw [feed_reject (cs1_bad hlf)]; rfl
  rename_i hlf
  simp only [Decidable.not_not] at hlf
  subst hlf
  rw [feed_nothing cs1_lf]
  split
  · -- the last chunk
    rename_i h0
    simp only [h0]
    split
    · rfl
    rename_i c3 L4
    split
    · rename_i h3; rw [feed_reject (cs5_bad h3)]; rfl
    rename_i h3
    simp only [Decidable.not_not] at h3
    subst h3
    rw [feed_nothing cs5]
    split
    · rfl
    rename_i c4 L5
    split
    · rename_i h4; rw [feed_reject (cs6_bad h4)]; rfl
    rename_i h4
    simp only [Decidable.not_not] at h4
    subst h4
    rw [feed_done cs6]
    simp only [FR.view, DR.shift, List.append_nil, DR.done.injEq, true_and]
    omega
  rename_i hnz
  split
  · rename_i hshort
    exact feed_view_shift_more (feed_data_short _ _ _ _ _ _ hshort)
  rename_i hlong
  have hDl : (L3.take (hexVal 0 (L.takeWhile isHex))).length = (hexVal 0 (L.takeWhile isHex)) := by simp; omega
  have hDne : L3.take (hexVal 0 (L.takeWhile isHex)) ≠ [] := by
    intro h; rw [h] at hDl; simp at hDl; omega
  conv => lhs; rw [← List.take_append_drop (hexVal 0 (L.takeWhile isHex)) L3]
  rw [feed_data' _ _ _ mb (t + (hexVal 0 (L.takeWhile isHex))) acc _ hDne hDl]
  split
  · rename_i hdr; conv => lhs; rw [hdr]
    rfl
  rename_i c3 L4 hdr
  conv => lhs; rw [hdr]
  split
  · rename_i h3; rw [feed_reject (cs3_bad h3)]; rfl
  rename_i h3
  simp only [Decidable.not_not] at h3
  subst h3
  rw [feed_nothing cs3]
  split
  · rfl
  rename_i c4 L5 hdr
  split
  · rename_i h4; rw [feed_reject (cs4_bad h4)]; rfl
  rename_i h4
  simp only [Decidable.not_not] at h4
  subst h4
  rw [feed_nothing cs4]
  have hl5 : L5.length < L.length := hlen5 L5 hd (by
    have b := congrArg List.length hdr
    simp only [List.length_cons, List.length_drop] at b
    simp only [List.length_cons]; omega)
  rw [ih L5.length hl5 L5 (t + (hexVal 0 (L.takeWhile isHex))) _ _ (by omega) rfl, shift_shift]
  congr 1
  omega

/-- Folding `chunkStep` over `L` from `cStart` gives `dechunk L`. -/
theorem feed_dechunk (L : List Nat) (mb : Nat) : (feed cStart L mb [] 0).view = dechunk L mb := by
  have := feed_chunks L mb 0 [] 0 (Nat.zero_le _)
  rw [shift_nil] at this
  exact this

/-- The end of a chunked body depends only on the bytes before it: if
`dechunk` finds the body ends after `k` bytes, then it does so whatever
follows those `k` bytes. -/
theorem dechunk_local (L : List Nat) (mb : Nat) (p : List Nat) (k : Nat) (h : dechunk L mb = .done p k) :
    k ≤ L.length ∧ ∀ R, dechunk (L.take k ++ R) mb = .done p k := by
  rw [← feed_dechunk] at h
  cases hf : feed cStart L mb [] 0 with
  | more s a => rw [hf] at h; cases h
  | reject s => rw [hf] at h; cases h
  | done p' k' =>
    rw [hf] at h
    simp only [FR.view, DR.done.injEq] at h
    obtain ⟨rfl, rfl⟩ := h
    obtain ⟨-, h2, h3⟩ := feed_local _ _ _ _ _ _ _ hf
    simp only [Nat.sub_zero] at h2 h3
    refine ⟨h2, fun R => ?_⟩
    rw [← feed_dechunk, h3 R]; rfl

end h5i_app_h1.Spec
