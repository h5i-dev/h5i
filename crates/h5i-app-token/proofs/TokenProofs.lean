import H5iAppToken
import TokenSpec
import H5iAppLib.Basic
/-! The extracted token code computes `TokenSpec`. -/
open Aeneas Aeneas.Std Result h5i_app_token h5i_app_token.Spec H5iAppLib

namespace h5i_app_token.Proofs

/-- Byte values of a slice or vector. -/
abbrev B (l : List U8) : List Nat := l.map (·.val)

/-- `next_dot` as a list function. -/
def nd (L : List Nat) (i : Nat) : Nat :=
  if L.length ≤ i then L.length else i + (splitDot (L.drop i)).1.length

theorem splitDot_fst_len_le (L : List Nat) : (splitDot L).1.length ≤ L.length := by
  induction L with
  | nil => simp [splitDot]
  | cons c L ih =>
    by_cases hc : c = 46
    · simp [splitDot, hc]
    · rw [splitDot_cons c L hc]; simp; omega

theorem nd_cons_dot (L : List Nat) (i : Nat) (hi : i < L.length) (hc : L[i] = 46) : nd L i = i := by
  unfold nd
  rw [if_neg (by omega), List.drop_eq_getElem_cons hi, hc]
  simp [splitDot]

theorem nd_cons_ne (L : List Nat) (i : Nat) (hi : i < L.length) (hc : L[i] ≠ 46) :
    nd L i = nd L (i + 1) := by
  unfold nd
  rw [if_neg (by omega), List.drop_eq_getElem_cons hi, splitDot_cons _ _ hc]
  by_cases h : L.length ≤ i + 1
  · rw [if_pos h]
    have : L.drop (i + 1) = [] := List.drop_eq_nil_of_le h
    simp [this, splitDot]; omega
  · rw [if_neg h]; simp; omega

theorem dot_val : DOT.val = 46 := by unfold DOT; rfl

theorem u8_ne_dot {c : U8} (h : ¬c = DOT) : c.val ≠ 46 := by
  intro hv; apply h
  have := dot_val
  exact UScalar.eq_of_val_eq (by omega)

@[step]
theorem next_dot_loop_spec (s : Slice U8) (i : Usize) :
    next_dot_loop s i ⦃ j => j.val = nd (B s.val) i.val ⦄ := by
  unfold next_dot_loop
  apply loop.spec_decr_nat (measure := fun (j : Usize) => s.length + 1 - j.val)
    (inv := fun j => nd (B s.val) i.val = nd (B s.val) j.val)
  · intro j hj
    unfold next_dot_loop.body
    step*
    · have hl : j.val < (B s.val).length := by simp; scalar_tac
      rw [hj, nd_cons_dot _ _ hl (by simp only [B, List.getElem_map]; rw [← i2_post, ‹i2 = DOT›, dot_val])]
    · refine ⟨?_, by scalar_tac⟩
      have hl : j.val < (B s.val).length := by simp; scalar_tac
      rw [hj, nd_cons_ne _ _ hl (by simp only [B, List.getElem_map]; rw [← i2_post]; exact u8_ne_dot ‹¬i2 = DOT›), i3_post]
    · rw [hj]; unfold nd; rw [if_pos (by simp; scalar_tac)]; simp
  · rfl

def slice (L : List Nat) (a b : Nat) : List Nat := (L.drop a).take (b - a)

theorem slice_cons (L : List Nat) (a b : Nat) (h1 : a < b) (h2 : b ≤ L.length) :
    slice L a b = L[a]'(by omega) :: slice L (a + 1) b := by
  unfold slice
  rw [List.drop_eq_getElem_cons (by omega)]
  have : b - a = (b - (a + 1)) + 1 := by omega
  rw [this, List.take_succ_cons]

theorem slice_nil (L : List Nat) (a b : Nat) (h : b ≤ a) : slice L a b = [] := by
  unfold slice; rw [show b - a = 0 by omega]; simp

theorem slice_two (L : List Nat) (a b : Nat) (h1 : a + 2 ≤ b) (h2 : b ≤ L.length) :
    slice L a b = L[a]'(by omega) :: L[a + 1]'(by omega) :: slice L (a + 2) b := by
  rw [slice_cons _ _ _ (by omega) h2, slice_cons _ _ _ (by omega) h2]

theorem slice_length (L : List Nat) (a b : Nat) (h : b ≤ L.length) :
    (slice L a b).length = b - a := by
  unfold slice; simp; omega

def q : Nat := 1844674407370955161

/-- The decimal loop, from accumulator `acc`. -/
def decRun (acc : Nat) : List Nat → Option Nat
  | [] => some acc
  | c :: r =>
    if c < 48 ∨ 57 < c then none
    else if q < acc ∨ (acc = q ∧ 5 < c - 48) then none
    else decRun (acc * 10 + (c - 48)) r

def foldFrom (acc : Nat) (l : List Nat) : Nat := l.foldl (fun a c => a * 10 + (c - 48)) acc

theorem foldFrom_ge (acc : Nat) (l : List Nat) : acc ≤ foldFrom acc l := by
  induction l generalizing acc with
  | nil => simp [foldFrom]
  | cons c l ih =>
    have := ih (acc * 10 + (c - 48))
    simp only [foldFrom, List.foldl_cons] at *
    omega

theorem decRun_eq (acc : Nat) (hacc : acc ≤ u64max) (l : List Nat) :
    decRun acc l = if l.all isDigit ∧ foldFrom acc l ≤ u64max then some (foldFrom acc l) else none := by
  induction l generalizing acc with
  | nil => simp [decRun, foldFrom, hacc]
  | cons c l ih =>
    have hge := foldFrom_ge (acc * 10 + (c - 48)) l
    simp only [decRun, List.all_cons, isDigit, foldFrom, List.foldl_cons] at *
    by_cases hd : c < 48 ∨ 57 < c
    · rw [if_pos hd, if_neg]; simp; omega
    · rw [if_neg hd]
      by_cases ho : q < acc ∨ (acc = q ∧ 5 < c - 48)
      · rw [if_pos ho, if_neg]; unfold q u64max at *; omega
      · rw [if_neg ho, ih _ (by unfold q u64max at *; omega)]
        have : decide (48 ≤ c ∧ c ≤ 57) = true := by simp; omega
        simp [this]

@[step]
theorem parse_dec_loop1_spec (s : Slice U8) (e : Usize) (acc : U64) (i : Usize)
    (he : e.val ≤ s.length) (hi : i.val ≤ e.val) :
    parse_dec_loop1 s e acc i ⦃ r => r.map (·.val) = decRun acc.val (slice (B s.val) i.val e.val) ⦄ := by
  unfold parse_dec_loop1
  apply loop.spec_decr_nat (measure := fun (x : U64 × Usize) => e.val - x.2.val)
    (inv := fun x => i.val ≤ x.2.val ∧ x.2.val ≤ e.val ∧
      decRun acc.val (slice (B s.val) i.val e.val) = decRun x.1.val (slice (B s.val) x.2.val e.val))
  · rintro ⟨a, j⟩ ⟨h1, h2, h3⟩
    simp only at h1 h2 h3 ⊢
    unfold parse_dec_loop1.body
    step*
    all_goals (try have hdv : d.val = c.val - 48 := by rw [d_post, U8.cast_U64_val_eq]; scalar_tac)
    all_goals first
      | (rw [h3, slice_nil _ _ _ (by scalar_tac)]; simp [decRun])
      | (rw [h3, slice_cons _ _ _ (by scalar_tac) (by simp; scalar_tac)]
         simp only [B, List.getElem_map, ← c_post, decRun]
         split_ifs <;> first | (simp; done) | (exfalso; unfold q at *; scalar_tac))
      | (refine ⟨by scalar_tac, by scalar_tac, ?_, by scalar_tac⟩
         rw [h3, slice_cons _ _ _ (by scalar_tac) (by simp; scalar_tac), i3_post]
         simp only [B, List.getElem_map, ← c_post, decRun]
         rw [if_neg (by scalar_tac), if_neg (by unfold q; scalar_tac)]
         congr 1; scalar_tac)
  · simp [hi]

@[step]
theorem parse_dec_loop0_spec (s : Slice U8) (e : Usize) (acc : U64) (i : Usize)
    (he : e.val ≤ s.length) (hi : i.val ≤ e.val) :
    parse_dec_loop0 s e acc i ⦃ r => r.map (·.val) = decRun acc.val (slice (B s.val) i.val e.val) ⦄ := by
  unfold parse_dec_loop0
  apply loop.spec_decr_nat (measure := fun (x : U64 × Usize) => e.val - x.2.val)
    (inv := fun x => i.val ≤ x.2.val ∧ x.2.val ≤ e.val ∧
      decRun acc.val (slice (B s.val) i.val e.val) = decRun x.1.val (slice (B s.val) x.2.val e.val))
  · rintro ⟨a, j⟩ ⟨h1, h2, h3⟩
    simp only at h1 h2 h3 ⊢
    unfold parse_dec_loop0.body
    step*
    all_goals (try have hdv : d.val = c.val - 48 := by rw [d_post, U8.cast_U64_val_eq]; scalar_tac)
    all_goals first
      | (rw [h3, slice_nil _ _ _ (by scalar_tac)]; simp [decRun])
      | (rw [h3, slice_cons _ _ _ (by scalar_tac) (by simp; scalar_tac)]
         simp only [B, List.getElem_map, ← c_post, decRun]
         split_ifs <;> first | (simp; done) | (exfalso; unfold q at *; scalar_tac))
      | (refine ⟨by scalar_tac, by scalar_tac, ?_, by scalar_tac⟩
         rw [h3, slice_cons _ _ _ (by scalar_tac) (by simp; scalar_tac), i3_post]
         simp only [B, List.getElem_map, ← c_post, decRun]
         rw [if_neg (by scalar_tac), if_neg (by unfold q; scalar_tac)]
         congr 1; scalar_tac)
  · simp [hi]

theorem decVal_run (l : List Nat) (h1 : l ≠ []) (h2 : ¬(l.head? = some 48 ∧ 1 < l.length)) :
    decVal l = decRun 0 l := by
  rw [decRun_eq 0 (by unfold u64max; omega)]
  unfold decVal
  rw [if_neg h1, if_neg h2]
  rfl

@[step]
theorem parse_dec_spec (s : Slice U8) (a b : Usize) :
    parse_dec s a b ⦃ r => r.map (·.val) =
      if b.val ≤ s.length then decVal (slice (B s.val) a.val b.val) else none ⦄ := by
  unfold parse_dec
  step*
  · have := slice_nil (B s.val) a.val b.val (by scalar_tac)
    simp [this, decVal]
  · rw [if_neg (by scalar_tac)]; rfl
  · have hs := slice_cons (B s.val) a.val b.val (by scalar_tac) (by simp; scalar_tac)
    have hl := slice_length (B s.val) a.val b.val (by simp; scalar_tac)
    rw [if_pos (by scalar_tac)]
    simp only [B, List.getElem_map, ← i1_post, ‹i1 = 48#u8›] at hs
    unfold decVal
    rw [if_neg (by simp [hs]), if_pos ⟨by simp [hs], by scalar_tac⟩]
    rfl
  · have hs := slice_cons (B s.val) a.val b.val (by scalar_tac) (by simp; scalar_tac)
    have h1 := slice_nil (B s.val) (a.val + 1) b.val (by scalar_tac)
    simp only [B, List.getElem_map, ← i1_post, ‹i1 = 48#u8›, h1] at hs
    rw [r_post, if_pos (by scalar_tac), hs]
    simp [decRun, decVal, fold, q, u64max, isDigit]
  · have hs := slice_cons (B s.val) a.val b.val (by scalar_tac) (by simp; scalar_tac)
    simp only [B, List.getElem_map, ← i1_post] at hs
    rw [r_post, if_pos (by scalar_tac), decVal_run _ (by simp [hs])]
    rintro ⟨hh, _⟩
    simp only [hs, List.head?_cons, Option.some.injEq] at hh
    exact ‹¬i1 = 48#u8› (UScalar.eq_of_val_eq (by simpa using hh))

@[step]
theorem hex_value_spec (c : U8) : hex_value c ⦃ v => v.val = hexv c.val ⦄ := by
  unfold hex_value
  step*
  all_goals (unfold hexv; split_ifs <;> scalar_tac)

theorem hexVal_odd : ∀ l : List Nat, l.length % 2 = 1 → hexVal l = none
  | [], h => by simp at h
  | [_], _ => rfl
  | _ :: _ :: r, h => by
    simp only [hexVal]
    split <;> simp [hexVal_odd r (by simp at h; omega)]

@[step]
theorem parse_hex_loop_spec (s : Slice U8) (e : Usize) (out : alloc.vec.Vec U8) (i : Usize)
    (he : e.val ≤ s.length) (hi : i.val ≤ e.val) (hev : (e.val - i.val) % 2 = 0)
    (ho : out.length ≤ i.val) :
    parse_hex_loop s e out i ⦃ r =>
      r.map (fun v => B v.val) = (hexVal (slice (B s.val) i.val e.val)).map (B out.val ++ ·) ⦄ := by
  unfold parse_hex_loop
  apply loop.spec_decr_nat (measure := fun (x : alloc.vec.Vec U8 × Usize) => e.val - x.2.val)
    (inv := fun x => i.val ≤ x.2.val ∧ x.2.val ≤ e.val ∧ (e.val - x.2.val) % 2 = 0 ∧
      x.1.length ≤ x.2.val ∧
      (hexVal (slice (B s.val) i.val e.val)).map (B out.val ++ ·) =
        (hexVal (slice (B s.val) x.2.val e.val)).map (B x.1.val ++ ·))
  · rintro ⟨o, j⟩ ⟨h1, h2, h3, h4, h5⟩
    simp only at h1 h2 h3 h4 h5 ⊢
    unfold parse_hex_loop.body
    step*
    all_goals (try have hs := slice_two (B s.val) j.val e.val (by scalar_tac) (by simp; scalar_tac))
    all_goals (try simp only [B, List.getElem_map, ← i1_post, ← i2_post, ← i3_post] at hs)
    · rw [h5, hs]; simp only [hexVal]
      rw [if_neg (by intro h; scalar_tac)]; simp
    · rw [h5, hs]; simp only [hexVal]
      rw [if_neg (by intro h; scalar_tac)]; simp
    · refine ⟨by scalar_tac, by scalar_tac, by scalar_tac, by simp [out1_post]; scalar_tac, ?_⟩
      rw [h5, hs, i6_post]; simp only [hexVal]
      rw [if_pos ⟨by scalar_tac, by scalar_tac⟩]
      simp only [Option.map_map, Function.comp_def, out1_post, B, List.map_append, List.map_cons,
        List.map_nil, List.append_assoc, List.cons_append, List.nil_append]
      congr 3
      scalar_tac
    · rw [h5, slice_nil _ _ _ (by scalar_tac)]; simp [hexVal]
  · simp [hi, hev, ho]

@[step]
theorem parse_hex_spec (s : Slice U8) (a b : Usize) :
    parse_hex s a b ⦃ r => r.map (fun v => B v.val) =
      if a.val ≤ b.val ∧ b.val ≤ s.length then hexVal (slice (B s.val) a.val b.val) else none ⦄ := by
  unfold parse_hex
  step*
  · rw [if_neg (by scalar_tac)]; rfl
  · rw [if_neg (by scalar_tac)]; rfl
  · rw [if_pos ⟨by scalar_tac, by scalar_tac⟩, hexVal_odd]
    · rfl
    · rw [slice_length _ _ _ (by simp; scalar_tac)]; scalar_tac
  · rw [r_post, if_pos ⟨by scalar_tac, by scalar_tac⟩]; simp

@[step]
theorem copy_prefix_loop_spec (s : Slice U8) (e : Usize) (out : alloc.vec.Vec U8) (i : Usize)
    (ho : out.length ≤ i.val) :
    copy_prefix_loop s e out i ⦃ v => B v.val = B out.val ++ slice (B s.val) i.val (min e.val s.length) ⦄ := by
  unfold copy_prefix_loop
  apply loop.spec_decr_nat (measure := fun (x : alloc.vec.Vec U8 × Usize) => s.length - x.2.val)
    (inv := fun x => x.1.length ≤ x.2.val ∧
      B out.val ++ slice (B s.val) i.val (min e.val s.length) =
        B x.1.val ++ slice (B s.val) x.2.val (min e.val s.length))
  · rintro ⟨o, j⟩ ⟨h1, h2⟩
    simp only at h1 h2 ⊢
    unfold copy_prefix_loop.body
    step*
    · refine ⟨by simp [out1_post]; scalar_tac, ?_, by scalar_tac⟩
      rw [h2, slice_cons _ _ _ (by scalar_tac) (by simp), i3_post]
      simp [out1_post, B, i2_post]
    · rw [h2, slice_nil _ _ _ (by scalar_tac)]; simp
    · rw [h2, slice_nil _ _ _ (by scalar_tac)]; simp
  · simp [ho]

@[step]
theorem copy_prefix_spec (s : Slice U8) (e : Usize) :
    copy_prefix s e ⦃ v => B v.val = (B s.val).take e.val ⦄ := by
  unfold copy_prefix
  step*
  rw [v_post]; simp [slice]

theorem nd_le (L : List Nat) (i : Nat) : nd L i ≤ L.length := by
  unfold nd
  split
  · omega
  · have := splitDot_fst_len_le (L.drop i); simp at this; omega

theorem nd_ge (L : List Nat) (i : Nat) (h : i ≤ L.length) : i ≤ nd L i := by
  unfold nd; split <;> omega

/-- `parse`, with indices, over byte values. -/
def parseIdx (L : List Nat) : Option Tok :=
  let n := L.length
  let d0 := nd L 0
  if d0 ≠ 2 ∨ L[0]? ≠ some 118 ∨ L[1]? ≠ some 49 then none else
  let d1 := nd L (d0 + 1)
  if d1 = n then none else
  let d2 := nd L (d1 + 1)
  if d2 = n then none else
  let d3 := nd L (d2 + 1)
  if d3 = n then none else
  if nd L (d3 + 1) ≠ n then none else
  match decVal (slice L (d0 + 1) d1), decVal (slice L (d1 + 1) d2),
    decVal (slice L (d2 + 1) d3), hexVal (slice L (d3 + 1) n) with
  | some t, some u, some e, some sig => some ⟨t, u, e, L.take d3, sig⟩
  | _, _, _, _ => none

def view (p : Parsed) : Tok := ⟨p.tenant.val, p.user.val, p.exp.val, B p.payload.val, B p.sig.val⟩

theorem byte_at (tok : Slice U8) (k : Nat) (c : U8) (hk : k < tok.length) (h : c = tok.val[k]) :
    (B tok.val)[k]? = some c.val := by
  subst h; simp [B, List.getElem?_eq_getElem (by simpa using hk)]

theorem opt_eq {α β : Type} {o : Option α} {f : α → β} {x : Option β} {c : Prop} [Decidable c]
    (hc : c) (h : o.map f = if c then x else none) : x = o.map f := by
  rw [h, if_pos hc]

theorem usize_eq_iff (x y : Usize) : x = y ↔ x.val = y.val := by
  constructor
  · rintro rfl; rfl
  · intro h; scalar_tac

theorem usize_not_bne (x y : Usize) : ¬ (x != y) = true ↔ x.val = y.val := by
  rw [← usize_eq_iff]; simp [bne]; exact (usize_eq_iff x y).symm

set_option maxHeartbeats 2000000 in
theorem parse_idx (tok : Slice U8) : parse tok ⦃ r => r.map view = parseIdx (B tok.val) ⦄ := by
  unfold parse
  have hnd := nd_le (B tok.val)
  have hlen : (B tok.val).length = tok.length := by simp
  step*
  all_goals (try simp only [Bool.not_eq_true, bne_eq_false_iff_eq, bne_iff_ne, ne_eq] at *)
  all_goals (try have hd0 : d0.val ≤ tok.length := by rw [d0_post, ← hlen]; exact hnd _)
  all_goals (try have hb0 := byte_at tok 0 i (by scalar_tac) i_post)
  all_goals (try have hb1 := byte_at tok 1 i1 (by scalar_tac) i1_post)
  all_goals (try (rw [i2_post] at d1_post))
  all_goals (try (rw [i3_post] at d2_post))
  all_goals (try (rw [i4_post] at d3_post))
  all_goals (try (rw [i5_post] at i6_post))
  all_goals (try have hd1 : d1.val ≤ tok.length := by rw [d1_post, ← hlen]; exact hnd _)
  all_goals (try have hd2 : d2.val ≤ tok.length := by rw [d2_post, ← hlen]; exact hnd _)
  all_goals (try have hd3 : d3.val ≤ tok.length := by rw [d3_post, ← hlen]; exact hnd _)
  all_goals (try have e1 := opt_eq hd1 o_post)
  all_goals (try have e2 := opt_eq hd2 o1_post)
  all_goals (try have e3 := opt_eq hd3 o2_post)
  all_goals (try have e4 := opt_eq (And.intro (show i5.val ≤ tok.len.val by scalar_tac) (le_refl _)) o3_post)
  all_goals (try rw [show tok.len.val = tok.length from by simp] at e4)
  all_goals (unfold parseIdx; simp only [hlen])
  all_goals (try rw [← d0_post])
  all_goals (try rw [← d1_post])
  all_goals (try rw [← d2_post])
  all_goals (try rw [← d3_post])
  all_goals (try rw [← i6_post])
  all_goals (try rw [← i2_post, e1])
  all_goals (try rw [← i3_post, e2])
  all_goals (try rw [← i4_post, e3])
  all_goals (try rw [← i5_post, e4])
  all_goals (try rw [hb0])
  all_goals (try rw [hb1])
  all_goals (try subst_vars)
  all_goals (try (have h0 : d0.val = 2 := by simpa using (usize_not_bne d0 2#usize).1 (by assumption)))
  all_goals (try simp only [Bool.not_eq_true, bne_eq_false_iff_eq, bne_iff_ne, ne_eq, not_not, u8_eq_iff, usize_eq_iff,
    Option.some.injEq, Option.map_none, Option.map_some, reduceCtorEq, UScalar.ofNatCore_val_eq, Slice.len_val] at *)
  all_goals (split_ifs <;> first
    | rfl
    | (exfalso; omega)
    | (exfalso; rename_i hdisj; rcases hdisj with h | h | h <;> contradiction)
    | skip)
  all_goals (simp only [view]; rw [v4_post])

theorem nd_split (L : List Nat) (i : Nat) (hi : i ≤ L.length) :
    (∃ r, splitDot (L.drop i) = (slice L i (nd L i), some r) ∧ nd L i < L.length ∧
      L.drop (nd L i + 1) = r ∧ L[nd L i]? = some 46) ∨
    (splitDot (L.drop i) = (slice L i (nd L i), none) ∧ nd L i = L.length) := by
  rcases hs : splitDot (L.drop i) with ⟨f, _ | r⟩
  · obtain ⟨_, hl⟩ := splitDot_none hs
    have hlen : f.length = L.length - i := by rw [← hl]; simp
    have hnd : nd L i = L.length := by
      unfold nd; split
      · rfl
      · rw [hs]; simp; omega
    right
    refine ⟨?_, hnd⟩
    rw [hnd]; unfold slice; rw [← hl]; simp
  · obtain ⟨_, hl⟩ := splitDot_some hs
    have hlen : L.length - i = f.length + 1 + r.length := by
      have := congrArg List.length hl; simp at this; omega
    have hnd : nd L i = i + f.length := by
      unfold nd; rw [if_neg (by omega), hs]
    left
    refine ⟨r, ?_, by omega, ?_, ?_⟩
    · rw [hnd]; unfold slice; rw [hl]; simp
    · rw [hnd, show i + f.length + 1 = i + (f.length + 1) by omega, ← List.drop_drop, hl]; simp
    · have := congrArg (·[f.length]?) hl
      simp only [List.getElem?_drop] at this
      rw [hnd, this]; simp

theorem take_slice (L : List Nat) (a b : Nat) (h : a ≤ b) : L.take b = L.take a ++ slice L a b := by
  unfold slice; rw [← List.take_add]; congr 1; omega

theorem take_dot (L : List Nat) (d : Nat) (h : L[d]? = some 46) : L.take (d + 1) = L.take d ++ [46] := by
  rw [List.take_add_one, h]; rfl

theorem slice_v1 (L : List Nat) (d : Nat) (hd : d ≤ L.length) :
    slice L 0 d = [118, 49] ↔ (d = 2 ∧ L[0]? = some 118 ∧ L[1]? = some 49) := by
  constructor
  · intro h
    have hl := congrArg List.length h
    rw [slice_length _ _ _ hd] at hl
    have h2 : d = 2 := by simpa using hl
    subst h2
    rw [slice_two _ _ _ (by omega) hd, slice_nil _ _ _ (le_refl _)] at h
    simp only [List.cons.injEq] at h
    refine ⟨rfl, ?_, ?_⟩
    · rw [List.getElem?_eq_getElem (show 0 < L.length by omega), h.1]
    · rw [List.getElem?_eq_getElem (show 1 < L.length by omega), h.2.1]
  · rintro ⟨rfl, h0, h1⟩
    rw [slice_two _ _ _ (by omega) hd, slice_nil _ _ _ (le_refl _)]
    rw [List.getElem?_eq_getElem (show 0 < L.length by omega)] at h0
    rw [List.getElem?_eq_getElem (show 1 < L.length by omega)] at h1
    simp_all

theorem nd_of_ge (L : List Nat) (i : Nat) (h : L.length ≤ i) : nd L i = L.length := by
  unfold nd; simp [h]

theorem parseIdx_eq (L : List Nat) : parseIdx L = parseSpec L := by
  unfold parseIdx parseSpec
  have e0 := nd_split L 0 (Nat.zero_le _)
  rw [List.drop_zero] at e0
  rcases e0 with ⟨_, hs0, hlt0, rfl, hdot0⟩ | ⟨hs0, heq0⟩
  swap
  · rw [hs0]
    simp only [heq0]
    split_ifs with h1 h2 <;> first | rfl | (exfalso; rw [nd_of_ge _ _ (by omega)] at h2; exact h2 rfl)
  have e1 := nd_split L (nd L 0 + 1) (by omega)
  rcases e1 with ⟨_, hs1, hlt1, rfl, hdot1⟩ | ⟨hs1, heq1⟩
  swap
  · rw [hs0]; simp only; rw [hs1]; simp only
    split_ifs <;> rfl
  have e2 := nd_split L (nd L (nd L 0 + 1) + 1) (by omega)
  rcases e2 with ⟨_, hs2, hlt2, rfl, hdot2⟩ | ⟨hs2, heq2⟩
  swap
  · rw [hs0]; simp only; rw [hs1]; simp only; rw [hs2]; simp only
    split_ifs <;> first | rfl | (exfalso; omega)
  have e3 := nd_split L (nd L (nd L (nd L 0 + 1) + 1) + 1) (by omega)
  rcases e3 with ⟨_, hs3, hlt3, rfl, hdot3⟩ | ⟨hs3, heq3⟩
  swap
  · rw [hs0]; simp only; rw [hs1]; simp only; rw [hs2]; simp only; rw [hs3]; simp only
    split_ifs <;> first | rfl | (exfalso; omega)
  have e4 := nd_split L (nd L (nd L (nd L (nd L 0 + 1) + 1) + 1) + 1) (by omega)
  rw [hs0]; simp only; rw [hs1]; simp only; rw [hs2]; simp only; rw [hs3]; simp only
  rcases e4 with ⟨_, hs4, hlt4, _, _⟩ | ⟨hs4, heq4⟩
  · rw [hs4]; simp only
    split_ifs <;> first | rfl | (exfalso; omega)
  rw [hs4]; simp only
  rw [heq4]; simp only [slice_v1 L (nd L 0) (by omega)]
  have g1 := nd_ge L (nd L 0 + 1) (by omega)
  have g2 := nd_ge L (nd L (nd L 0 + 1) + 1) (by omega)
  have g3 := nd_ge L (nd L (nd L (nd L 0 + 1) + 1) + 1) (by omega)
  rw [take_slice L _ _ g3, take_dot L _ hdot2, take_slice L _ _ g2, take_dot L _ hdot1,
    take_slice L _ _ g1, take_dot L _ hdot0, take_slice L 0 _ (Nat.zero_le _)]
  simp only [List.take_zero, List.nil_append, List.append_assoc]
  split_ifs <;> first | rfl | (exfalso; omega) | (exfalso; tauto)

/-- What `parse` accepts is canonical: `enc tenant user exp` then `.hex(sig)`. -/
theorem parse_canonical (tok : Slice U8) :
    parse tok ⦃ r => ∀ p, r = some p →
      B p.payload.val = enc p.tenant.val p.user.val p.exp.val ∧
      B tok.val = joinS (B p.payload.val) (B p.sig.val) ⦄ := by
  apply WP.spec_mono (parse_idx tok)
  rintro r hr p rfl
  rw [parseIdx_eq] at hr
  obtain ⟨h1, h2, -⟩ := parse_canon hr.symm
  exact ⟨h1, h2⟩

/-- Equal signed payloads name the same tenant, user and expiry. -/
theorem payload_determines_identity {tok tok' : Slice U8} {p p' : Parsed}
    (h : parse tok = .ok (some p)) (h' : parse tok' = .ok (some p'))
    (hp : p.payload.val = p'.payload.val) :
    p.tenant = p'.tenant ∧ p.user = p'.user ∧ p.exp = p'.exp := by
  have c := parse_canonical tok
  have c' := parse_canonical tok'
  rw [h, WP.spec_ok] at c
  rw [h', WP.spec_ok] at c'
  have e := (c p rfl).1
  have e' := (c' p' rfl).1
  rw [hp, e'] at e
  have hb : ∀ x : U64, x.val ≤ u64max := fun x => by unfold u64max; scalar_tac
  obtain ⟨a, b, d⟩ := enc_injective (hb _) (hb _) (hb _) (hb _) (hb _) (hb _) e.symm
  exact ⟨by scalar_tac, by scalar_tac, by scalar_tac⟩

end h5i_app_token.Proofs
