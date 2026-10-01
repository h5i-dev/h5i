import H5iAppJson
import JsonSpec
import H5iAppLib.Basic
/-! The extracted writer computes `Spec.print`. -/
open Aeneas Aeneas.Std Result h5i_app_json h5i_app_json.Spec H5iAppLib

namespace h5i_app_json.Proofs

/-- Byte values of a byte list. -/
def B (l : List U8) : List Nat := l.map (·.val)

@[step]
theorem hex_digit_spec (n : U8) (h : n.val < 16) : hex_digit n ⦃ r => r.val = hexd n.val ⦄ := by
  unfold hex_digit hexd
  step*
  all_goals (simp_all; try scalar_tac)

theorem esc_cons (c : Nat) (l : List Nat) : esc (c :: l) = escByte c ++ esc l := by
  simp [esc]

@[step]
theorem push_escaped_byte_spec (o : alloc.vec.Vec U8) (c : U8)
    (h : o.length + (escByte c.val).length < Usize.max) :
    push_escaped_byte o c ⦃ r => B r.val = B o.val ++ escByte c.val ⦄ := by
  unfold push_escaped_byte
  rcases (show c.val = 34 ∨ c.val = 92 ∨ c.val < 32 ∨ (c.val ≠ 34 ∧ c.val ≠ 92 ∧ 32 ≤ c.val) by omega)
    with h1 | h1 | h1 | h1
  · rw [show escByte c.val = [92, 34] by simp [escByte, h1]] at h ⊢
    simp at h
    step*
    all_goals (simp_all [B, u8_eq_iff]; try scalar_tac)
  · rw [show escByte c.val = [92, 92] by simp [escByte, h1]] at h ⊢
    simp at h
    step*
    all_goals (simp_all [B, u8_eq_iff]; try scalar_tac)
  · rw [show escByte c.val = [92, 117, 48, 48, hexd (c.val / 16), hexd (c.val % 16)] by
      simp [escByte, h1, show c.val ≠ 34 by omega, show c.val ≠ 92 by omega]] at h ⊢
    simp at h
    step*
    all_goals (simp_all [B, u8_eq_iff]; try scalar_tac)
  · rw [show escByte c.val = [c.val] by simp [escByte, h1]] at h ⊢
    simp at h
    step*
    all_goals (simp_all [B, u8_eq_iff]; try scalar_tac)

theorem B_drop (s : alloc.vec.Vec U8) (j : Nat) (h : j < s.length) :
    B (s.val.drop j) = s.val[j].val :: B (s.val.drop (j + 1)) := by
  show List.map _ (List.drop j s.val) = _
  rw [List.drop_eq_getElem_cons (by simpa using h)]; rfl

theorem B_len (v : alloc.vec.Vec U8) : (B v.val).length = v.length := by simp [B]

@[step]
theorem push_escaped_loop_spec (out s : alloc.vec.Vec U8) (i : Usize) (hi : i.val ≤ s.length)
    (hb : out.length + (esc (B (s.val.drop i.val))).length < Usize.max) :
    push_escaped_loop out s i ⦃ r => B r.val = B out.val ++ esc (B (s.val.drop i.val)) ⦄ := by
  unfold push_escaped_loop
  apply loop.spec_decr_nat (measure := fun (p : alloc.vec.Vec U8 × Usize) => s.length - p.2.val)
    (inv := fun p => p.2.val ≤ s.length ∧
      B p.1.val ++ esc (B (s.val.drop p.2.val)) = B out.val ++ esc (B (s.val.drop i.val)) ∧
      p.1.length + (esc (B (s.val.drop p.2.val))).length < Usize.max)
  · rintro ⟨o, j⟩ ⟨hj, heq, hlen⟩
    simp only at hj heq hlen
    unfold push_escaped_loop.body
    by_cases hjs : j.val < s.length
    · rw [B_drop s j.val hjs, esc_cons] at heq hlen
      simp only [List.length_append] at hlen
      step*
      · refine ⟨by scalar_tac, ?_, ?_, by scalar_tac⟩
        · rw [i3_post, out1_post, i2_post, List.append_assoc]; exact heq
        · have e := congrArg List.length out1_post
          rw [List.length_append, B_len, B_len, i2_post] at e
          rw [i3_post]; omega
    · step*
      have : s.val.drop j.val = [] := List.drop_eq_nil_of_le (by scalar_tac)
      rw [this] at heq; simpa [B, esc] using heq
  · exact ⟨hi, rfl, hb⟩

@[step]
theorem push_escaped_spec (out s : alloc.vec.Vec U8)
    (hb : out.length + (esc (B s.val)).length < Usize.max) :
    push_escaped out s ⦃ r => B r.val = B out.val ++ esc (B s.val) ∧
      r.length = out.length + (esc (B s.val)).length ⦄ := by
  unfold push_escaped
  apply WP.spec_mono (push_escaped_loop_spec out s 0#usize (by simp) (by simpa using hb))
  intro r h
  have h' : B r.val = B out.val ++ esc (B s.val) := by simpa using h
  refine ⟨h', ?_⟩
  have e := congrArg List.length h'
  simpa [B] using e

/-- Digits after the first, little-endian. -/
def rdf (m : Nat) : List Nat := if m = 0 then [] else m % 10 :: rdf (m / 10)

theorem revDigits_eq (n : Nat) : revDigits n = n % 10 :: rdf (n / 10) := by
  induction n using Nat.strong_induction_on with
  | _ n ih =>
    rw [revDigits]
    split
    · rw [rdf]; simp; omega
    · rw [ih (n / 10) (by omega), rdf.eq_def (n / 10)]; simp; omega

theorem rdf_len (m k : Nat) (h : m < 10 ^ k) : (rdf m).length ≤ k := by
  induction k generalizing m with
  | zero => rw [rdf]; simp at h; simp [h]
  | succ k ih =>
    rw [rdf]; split
    · simp
    · simp only [List.length_cons]; have := ih (m / 10) (by rw [pow_succ] at h; omega); omega

theorem rdf_u64 (m : U64) : (rdf m.val).length ≤ 20 :=
  rdf_len _ _ (by have := m.hBounds; simp [U64.size] at this ⊢; omega)

theorem rdf_pos (k : Nat) (h : 0 < k) : rdf k = k % 10 :: rdf (k / 10) := by
  rw [rdf]; simp; omega

@[step]
theorem push_dec_loop0_spec (rev : alloc.vec.Vec U8) (m : U64) (hb : rev.length + 20 < Usize.max) :
    push_dec_loop0 rev m ⦃ r => B r.val = B rev.val ++ (rdf m.val).map (48 + ·) ∧
      r.length ≤ rev.length + 20 ⦄ := by
  unfold push_dec_loop0
  apply loop.spec_decr_nat (measure := fun (p : alloc.vec.Vec U8 × U64) => p.2.val)
    (inv := fun p => B p.1.val ++ (rdf p.2.val).map (48 + ·) = B rev.val ++ (rdf m.val).map (48 + ·) ∧
       p.1.length + (rdf p.2.val).length ≤ rev.length + 20)
  · rintro ⟨r, k⟩ ⟨heq, hl⟩
    simp only at heq hl
    unfold push_dec_loop0.body
    step*
    · have hk : 0 < k.val := by scalar_tac
      have hd : d.val = k.val % 10 := by
        rw [d_post, UScalar.cast_val_mod_pow_of_inBounds_eq _ _ (by simp; omega), i_post]
      rw [rdf_pos k.val hk] at heq hl
      refine ⟨?_, ?_, by scalar_tac⟩
      · rw [← heq, m1_post]; simp [B, rev1_post, i1_post, hd]
      · have := congrArg List.length rev1_post
        simp at this hl; rw [m1_post]; simp only [alloc.vec.Vec.length] at *; omega
    · have hk : k.val = 0 := by scalar_tac
      rw [hk, rdf] at heq hl
      simp at heq hl
      exact ⟨heq, hl⟩
  · exact ⟨rfl, by simp only; have := rdf_u64 m; omega⟩

theorem B_take_succ (rev : alloc.vec.Vec U8) (k : Nat) (h : k < rev.length) :
    (B (rev.val.take (k + 1))).reverse = rev.val[k].val :: (B (rev.val.take k)).reverse := by
  unfold B
  rw [List.take_succ_eq_append_getElem (by simpa using h), List.map_append, List.reverse_append]
  rfl

@[step]
theorem push_dec_loop1_spec (out rev : alloc.vec.Vec U8) (i : Usize) (hi : i.val ≤ rev.length)
    (hb : out.length + i.val < Usize.max) :
    push_dec_loop1 out rev i ⦃ r => B r.val = B out.val ++ (B (rev.val.take i.val)).reverse ⦄ := by
  unfold push_dec_loop1
  apply loop.spec_decr_nat (measure := fun (p : alloc.vec.Vec U8 × Usize) => p.2.val)
    (inv := fun p => p.2.val ≤ i.val ∧
      B p.1.val ++ (B (rev.val.take p.2.val)).reverse = B out.val ++ (B (rev.val.take i.val)).reverse ∧
      p.1.length + p.2.val = out.length + i.val)
  · rintro ⟨o, j⟩ ⟨hj, heq, hl⟩
    simp only at hj heq hl
    unfold push_dec_loop1.body
    step*
    · have hj0 : 0 < j.val := by clear heq; scalar_tac
      have e : j.val = i1.val + 1 := by clear heq; scalar_tac
      have hi1 : i1.val < rev.length := by omega
      refine ⟨by omega, ?_, ?_, by omega⟩
      · rw [← heq, e, B_take_succ rev i1.val hi1]
        rw [out1_post, i2_post]
        simp only [B, List.map_append, List.map_cons, List.map_nil, List.append_assoc, List.singleton_append]
      · have e := congrArg List.length out1_post
        simp only [List.length_append, List.length_singleton] at e
        simp only [alloc.vec.Vec.length] at hl ⊢
        omega
    · have : j.val = 0 := by scalar_tac
      rw [this] at heq; simpa [B] using heq
  · exact ⟨le_refl _, rfl, rfl⟩

theorem dec_eq (n : Nat) : dec n = (((n % 10) :: rdf (n / 10)).map (48 + ·)).reverse := by
  rw [dec, revDigits_eq]

@[step]
theorem push_dec_spec (out : alloc.vec.Vec U8) (n : U64) (hb : out.length + 21 < Usize.max) :
    push_dec out n ⦃ r => B r.val = B out.val ++ dec n.val ⦄ := by
  unfold push_dec
  step*
  have hd : d.val = n.val % 10 := by
    rw [d_post, UScalar.cast_val_mod_pow_of_inBounds_eq _ _ (by simp; omega), i_post]
  have hall : B (rev1.val.take rev1.len.val) = B rev1.val := by simp
  rw [r_post, hall, rev1_post, dec_eq, m_post]
  simp [B, rev_post, i1_post, hd]

/-- The spec token for an extracted token. -/
def toT : Tok → T
  | .Null => .null
  | .Bool b => .bool b
  | .Num n => .num n.val
  | .Str s => .str (B s.val)
  | .Key s => .key (B s.val)
  | .ArrOpen => .arrOpen
  | .ArrClose => .arrClose
  | .ObjOpen => .objOpen
  | .ObjClose => .objClose

@[step]
theorem ends_value_spec (t : Tok) : ends_value t ⦃ b => b = endsValue (toT t) ⦄ := by
  cases t <;> simp [ends_value, toT, endsValue]

@[step]
theorem is_close_spec (t : Tok) : is_close t ⦃ b => b = isClose (toT t) ⦄ := by
  cases t <;> simp [is_close, toT, isClose]

@[step]
theorem push_tok_spec (o : alloc.vec.Vec U8) (t : Tok)
    (hb : o.length + (tokBytes (toT t)).length + 21 < Usize.max) :
    push_tok o t ⦃ r => B r.val = B o.val ++ tokBytes (toT t) ⦄ := by
  unfold push_tok
  cases t with
  | Str s =>
    simp only [toT, tokBytes, List.length_cons, List.length_append, List.length_nil] at hb ⊢
    have h2 : o.length + 1 + (esc (B s.val)).length < Usize.max := by omega
    step*
    all_goals (have e1 := congrArg List.length out1_post; simp only [List.length_append, List.length_singleton] at e1)
    all_goals (try (simp only [alloc.vec.Vec.length] at *; omega))
    all_goals (simp_all [B])
  | Key s =>
    simp only [toT, tokBytes, List.length_cons, List.length_append, List.length_nil] at hb ⊢
    have h2 : o.length + 1 + (esc (B s.val)).length < Usize.max := by omega
    step*
    all_goals (have e1 := congrArg List.length out1_post; simp only [List.length_append, List.length_singleton] at e1)
    all_goals (try (simp only [alloc.vec.Vec.length] at *; omega))
    all_goals (try simp_all [B])
    all_goals ((try simp only [B, alloc.vec.Vec.length] at *); omega)
  | Num n =>
    simp only [toT, tokBytes] at hb ⊢
    step*
  | Bool b =>
    cases b
    all_goals (simp only [toT, tokBytes, List.length_cons, List.length_nil, alloc.vec.Vec.length] at hb ⊢)
    all_goals (step* <;> try omega)
    all_goals (try simp_all [B])
    all_goals omega
  | Null =>
    simp only [toT, tokBytes, List.length_cons, List.length_nil, alloc.vec.Vec.length] at hb ⊢
    all_goals (step* <;> try omega)
    all_goals (try simp_all [B])
    all_goals omega
  | _ =>
    simp only [toT, tokBytes, List.length_cons, List.length_nil, alloc.vec.Vec.length] at hb ⊢
    all_goals (step* <;> try omega)
    all_goals (try simp_all [B])

theorem fold_prefix (ys : List T) (st : List Nat × Bool) :
    ∃ t, (ys.foldl step st).1 = st.1 ++ t := by
  induction ys generalizing st with
  | nil => exact ⟨[], by simp⟩
  | cons y ys ih =>
    obtain ⟨t, ht⟩ := ih (step st y)
    refine ⟨(step st y).1.drop st.1.length ++ t, ?_⟩
    rw [List.foldl_cons, ht, ← List.append_assoc]
    congr 1
    simp [step]

/-- Output after `j` tokens, with one more token, is a prefix of the whole. -/
theorem take_succ_len (L : List T) (j : Nat) :
    ((L.take (j + 1)).foldl step ([], false)).1.length ≤ (print L).length := by
  obtain ⟨t, ht⟩ := fold_prefix (L.drop (j + 1)) ((L.take (j + 1)).foldl step ([], false))
  have : print L = ((L.take (j + 1)).foldl step ([], false)).1 ++ t := by
    rw [print, ← ht, ← List.foldl_append, List.take_append_drop]
  rw [this]; simp

theorem take_succ_fold (L : List T) (j : Nat) (h : j < L.length) :
    (L.take (j + 1)).foldl step ([], false) = step ((L.take j).foldl step ([], false)) L[j] := by
  rw [List.take_succ_eq_append_getElem h, List.foldl_append]; rfl

@[step]
theorem write_loop_spec (toks : alloc.vec.Vec Tok) (out : alloc.vec.Vec U8) (av : Bool) (i : Usize)
    (hi : i.val ≤ toks.length)
    (hst : (B out.val, av) = ((toks.val.map toT).take i.val).foldl step ([], false))
    (hb : (print (toks.val.map toT)).length + 21 < Usize.max) :
    write_loop toks out av i ⦃ r => B r.val = print (toks.val.map toT) ⦄ := by
  unfold write_loop
  apply loop.spec_decr_nat
    (measure := fun (p : alloc.vec.Vec U8 × Bool × Usize) => toks.length - p.2.2.val)
    (inv := fun p => p.2.2.val ≤ toks.length ∧
      (B p.1.val, p.2.1) = ((toks.val.map toT).take p.2.2.val).foldl step ([], false))
  · rintro ⟨o, a, j⟩ ⟨hj, hs⟩
    simp only at hj hs
    unfold write_loop.body
    by_cases hjl : j.val < toks.length
    · have hjL : j.val < (toks.val.map toT).length := by simpa using hjl
      have hnext := take_succ_fold _ _ hjL
      have hlen := take_succ_len (toks.val.map toT) j.val
      rw [hnext, ← hs] at hlen
      simp only [step, List.length_append, B_len, List.getElem_map] at hlen
      cases a
      all_goals simp only [Bool.false_eq_true, if_false, if_true, Bool.false_and, Bool.true_and] at hlen ⊢
      all_goals (step* <;> try (simp only [alloc.vec.Vec.length] at *; omega))
      all_goals (try (split <;> (step* <;> try (simp only [alloc.vec.Vec.length] at *; omega))))
      all_goals (try (
        have e := congrArg List.length out1_post
        subst_vars
        simp only [List.length_append, List.length_cons, List.length_nil, alloc.vec.Vec.length] at e hlen ⊢
        split at hlen <;> simp_all <;> omega))
      all_goals (
        refine ⟨by scalar_tac, ?_, by scalar_tac⟩
        rw [i2_post, hnext, ← hs]
        subst t_post
        simp_all [step, B, List.getElem_map])
    · step*
      have : j.val = toks.length := by scalar_tac
      rw [this, show toks.length = (toks.val.map toT).length by simp, List.take_length] at hs
      have := congrArg Prod.fst hs; simpa [print] using this
  · exact ⟨hi, hst⟩

/-- `write` prints exactly `Spec.print` when the output fits in a usize. -/
theorem write_spec (toks : alloc.vec.Vec Tok)
    (hb : (print (toks.val.map toT)).length + 21 < Usize.max) :
    write toks ⦃ r => B r.val = print (toks.val.map toT) ⦄ := by
  unfold write
  exact write_loop_spec toks _ false 0#usize (by simp) (by simp [B]) hb

/-- A string token's bytes cannot end its JSON string early. -/
theorem write_str_contained (s : alloc.vec.Vec U8) (rest : List Nat) :
    lexStr (esc (B s.val) ++ 34 :: rest) = some (B s.val, rest) :=
  lex_esc _ _ (fun c hc => by simp only [B, List.mem_map] at hc; obtain ⟨x, -, rfl⟩ := hc; scalar_tac)

/-- The `j`th string or key token sits quoted in the output and lexes back to itself. -/
theorem write_str_at (toks : alloc.vec.Vec Tok)
    (hb : (print (toks.val.map toT)).length + 21 < Usize.max)
    (j : Nat) (hj : j < toks.length) (s : alloc.vec.Vec U8) (hs : toks.val[j]'(by simpa using hj) = .Str s ∨ toks.val[j]'(by simpa using hj) = .Key s) :
    write toks ⦃ r => ∃ post,
      B r.val = (((toks.val.map toT).take j).foldl step ([], false)).1 ++
        (if (((toks.val.map toT).take j).foldl step ([], false)).2 then [44] else []) ++
        34 :: (esc (B s.val) ++ 34 :: post) ∧
      lexStr (esc (B s.val) ++ 34 :: post) = some (B s.val, post) ⦄ := by
  apply WP.spec_mono (write_spec toks hb)
  intro r hr
  set L := toks.val.map toT
  have hjL : j < L.length := by simpa [L] using hj
  obtain ⟨post, hpost⟩ := fold_prefix (L.drop (j + 1)) ((L.take (j + 1)).foldl step ([], false))
  rw [hr, print, ← List.take_append_drop (j + 1) L, List.foldl_append, hpost,
    take_succ_fold L j hjL]
  rcases hs with hs | hs
  · have hLj : L[j] = .str (B s.val) := by simp [L, hs, toT]
    refine ⟨post, ?_, write_str_contained s post⟩
    rw [hLj]; simp [step, isClose, tokBytes]
  · have hLj : L[j] = .key (B s.val) := by simp [L, hs, toT]
    refine ⟨58 :: post, ?_, write_str_contained s _⟩
    rw [hLj]; simp [step, isClose, tokBytes]

end h5i_app_json.Proofs
