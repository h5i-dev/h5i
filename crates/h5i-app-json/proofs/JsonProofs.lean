import H5iAppJson
import JsonSpec
import H5iAppLib.Basic
/-! The extracted writers compute `Spec.print` (`write`) and `Spec.tokBytes`
(the slice writers in `slice`). -/
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

/-! ## The slice writer -/

/-- `o` holds `bs` over `out`: the same length, `bs` first, then `out`'s bytes. -/
def W (o out : Slice U8) (bs : List Nat) : Prop :=
  o.length = out.length ∧ bs.length ≤ out.length ∧ B o.val = bs ++ (B out.val).drop bs.length

/-- Before anything is written at `pos`, `out` holds its own first `pos` bytes. -/
theorem W.start (out : Slice U8) (pos : Nat) (h : pos ≤ out.length) : W out out ((B out.val).take pos) :=
  ⟨rfl, by simp [B], by simp [B, Nat.min_eq_left (show pos ≤ out.val.length from h)]⟩

theorem W.set1 {o out : Slice U8} {bs : List Nat} (hW : W o out bs) (k : Usize) (hk : k.val = bs.length)
    (hlt : bs.length < out.length) (b : U8) : W (o.set k b) out (bs ++ [b.val]) := by
  obtain ⟨h1, h2, h3⟩ := hW
  have hol : bs.length < o.val.length := by simp only [Slice.length] at h1 hlt; omega
  refine ⟨by simp only [Slice.length, Slice.set_val_eq, List.length_set] at h1 ⊢; exact h1,
    by simp; omega, ?_⟩
  rw [Slice.set_val_eq, hk, List.set_eq_take_append_cons_drop, if_pos hol]
  have hb : B (o.val.take bs.length) = bs := by
    simp only [B] at h3 ⊢; rw [List.map_take, h3, List.take_left' rfl]
  have hd : B (o.val.drop (bs.length + 1)) = (B out.val).drop (bs.length + 1) := by
    simp only [B] at h3 ⊢; rw [List.map_drop, h3, ← List.drop_drop, List.drop_left' rfl, List.drop_drop]
  simp only [B, List.map_append, List.map_cons] at hb hd ⊢
  rw [hb, hd]
  simp

theorem B_length (s : Slice U8) : (B s.val).length = s.length := by simp [B, Slice.length]

@[step]
theorem put_byte_W (o out : Slice U8) (bs : List Nat) (pos : Usize) (b : U8)
    (hW : W o out bs) (hp : pos.val = bs.length) :
    slice.put_byte o pos b ⦃ r o' => match r with
      | none => True
      | some p => p.val = bs.length + 1 ∧ W o' out (bs ++ [b.val]) ⦄ := by
  unfold slice.put_byte
  have hl := hW.1
  step*
  have hlt : bs.length < out.length := by simp only [Slice.length] at *; scalar_tac
  refine ⟨by scalar_tac, ?_⟩
  rw [out1_post]
  exact hW.set1 pos hp hlt b

theorem put_escaped_byte_W (o out : Slice U8) (bs : List Nat) (pos : Usize) (c : U8)
    (hW : W o out bs) (hp : pos.val = bs.length) :
    slice.put_escaped_byte o pos c ⦃ r o' => match r with
      | none => True
      | some p => p.val = bs.length + (escByte c.val).length ∧ W o' out (bs ++ escByte c.val) ⦄ := by
  unfold slice.put_escaped_byte
  have hl := hW.1
  rcases (show c.val = 34 ∨ c.val = 92 ∨ c.val < 32 ∨ (c.val ≠ 34 ∧ c.val ≠ 92 ∧ 32 ≤ c.val) by omega)
    with h1 | h1 | h1 | h1
  · rw [show escByte c.val = [92, 34] by simp [escByte, h1]]
    step*
    have hroom : bs.length + 2 ≤ out.length := by
      have := Slice.len_val o; simp only [Slice.length] at *; scalar_tac
    refine ⟨by simp; scalar_tac, ?_⟩
    have w := (hW.set1 pos hp (by omega) 92#u8).set1 i3 (by simp only [List.length_append, List.length_singleton]; omega) (by simp only [List.length_append, List.length_singleton]; omega) 34#u8
    rw [out2_post, out1_post]; simpa using w
  · rw [show escByte c.val = [92, 92] by simp [escByte, h1]]
    step*
    have hroom : bs.length + 2 ≤ out.length := by
      have := Slice.len_val o; simp only [Slice.length] at *; scalar_tac
    refine ⟨by simp; scalar_tac, ?_⟩
    have w := (hW.set1 pos hp (by omega) 92#u8).set1 i3 (by simp only [List.length_append, List.length_singleton]; omega) (by simp only [List.length_append, List.length_singleton]; omega) 92#u8
    rw [out2_post, out1_post]; simpa using w
  · rw [show escByte c.val = [92, 117, 48, 48, hexd (c.val / 16), hexd (c.val % 16)] by
      simp [escByte, h1, show c.val ≠ 34 by omega, show c.val ≠ 92 by omega]]
    step*
    all_goals (try (subst_vars; simp only [Slice.set_length] at *; scalar_tac))
    have hroom : bs.length + 6 ≤ out.length := by
      have := Slice.len_val o; simp only [Slice.length] at *; scalar_tac
    refine ⟨by simp; scalar_tac, ?_⟩
    have w1 := hW.set1 pos hp (by omega) 92#u8
    have w2 := w1.set1 i3 (by simp only [List.length_append, List.length_singleton]; omega) (by simp only [List.length_append, List.length_singleton]; omega) 117#u8
    have w3 := w2.set1 i4 (by simp only [List.length_append, List.length_singleton]; omega) (by simp only [List.length_append, List.length_singleton]; omega) 48#u8
    have w4 := w3.set1 i5 (by simp only [List.length_append, List.length_singleton]; omega) (by simp only [List.length_append, List.length_singleton]; omega) 48#u8
    have w5 := w4.set1 i8 (by simp only [List.length_append, List.length_singleton]; omega) (by simp only [List.length_append, List.length_singleton]; omega) i7
    have w6 := w5.set1 i11 (by simp only [List.length_append, List.length_singleton]; omega) (by simp only [List.length_append, List.length_singleton]; omega) i10
    rw [out6_post, out5_post, out4_post, out3_post, out2_post, out1_post]
    simpa [i7_post, i10_post, i6_post, i9_post] using w6
  · rw [show escByte c.val = [c.val] by simp [escByte, h1]]
    have h34 : c ≠ 34#u8 := by intro h; apply h1.1; simp [h]
    have h92 : c ≠ 92#u8 := by intro h; apply h1.2.1; simp [h]
    have h32 : ¬ c < 32#u8 := by have := h1.2.2; scalar_tac
    simp only [h34, h92, h32, if_false]
    apply WP.spec_mono (put_byte_W o out bs pos c hW hp)
    rintro ⟨r, o'⟩ h
    simpa using h

theorem esc_take_succ (s : Slice U8) (i : Nat) (h : i < s.length) :
    esc (B (s.val.take (i + 1))) = esc (B (s.val.take i)) ++ escByte (s.val[i]'h).val := by
  unfold B esc
  rw [List.take_succ_eq_append_getElem h, List.map_append, List.flatMap_append]
  simp

theorem put_escaped_loop_W (o out s : Slice U8) (bs : List Nat) (pos i : Usize)
    (hW : W o out (bs ++ esc (B (s.val.take i.val))))
    (hp : pos.val = bs.length + (esc (B (s.val.take i.val))).length) (hi : i.val ≤ s.length) :
    slice.put_escaped_loop o s pos i ⦃ r o' => match r with
      | none => True
      | some p => p.val = bs.length + (esc (B s.val)).length ∧ W o' out (bs ++ esc (B s.val)) ⦄ := by
  unfold slice.put_escaped_loop
  apply loop.spec_decr_nat (measure := fun (x : Slice U8 × Usize × Usize) => s.length - x.2.2.val)
    (inv := fun x => x.2.2.val ≤ s.length ∧
      x.2.1.val = bs.length + (esc (B (s.val.take x.2.2.val))).length ∧
      W x.1 out (bs ++ esc (B (s.val.take x.2.2.val))))
  · rintro ⟨o1, at1, j⟩ ⟨hj, hat, hW1⟩
    simp only at hj hat hW1
    unfold slice.put_escaped_loop.body
    by_cases hjs : j.val < s.length
    · have hlt : j < s.len := by scalar_tac
      simp only [hlt, if_true]
      step
      step with put_escaped_byte_W o1 out (bs ++ esc (B (s.val.take j.val))) at1 i2 hW1 (by simpa using hat)
      split
      · simp
      · rename_i _ a
        simp only at o_post
        obtain ⟨ha, hWa⟩ := o_post
        step
        have e := esc_take_succ s j.val hjs
        refine ⟨by scalar_tac, ?_, ?_, by scalar_tac⟩
        · rw [i3_post, e]; simp only [List.length_append] at ha ⊢; rw [ha, i2_post]; omega
        · rw [i3_post, e, ← i2_post, ← List.append_assoc]; exact hWa
    · have hge : ¬ j < s.len := by scalar_tac
      simp only [hge, if_false, WP.spec_ok]
      have : s.val.take j.val = s.val := List.take_of_length_le (by simp only [Slice.length] at *; omega)
      rw [this] at hat hW1
      exact ⟨hat, hW1⟩
  · exact ⟨hi, hp, hW⟩

/-- `put_str` writes exactly the bytes of a string token at `pos`. -/
theorem put_str_W (o out s : Slice U8) (bs : List Nat) (pos : Usize)
    (hW : W o out bs) (hp : pos.val = bs.length) :
    slice.put_str o pos s ⦃ r o' => match r with
      | none => True
      | some p => p.val = bs.length + (tokBytes (.str (B s.val))).length ∧
          W o' out (bs ++ tokBytes (.str (B s.val))) ⦄ := by
  unfold slice.put_str slice.put_escaped
  step with put_byte_W o out bs pos 34#u8 hW hp
  split
  · simp
  rename_i _ a
  simp only at o_post
  obtain ⟨ha, hW1⟩ := o_post
  step with put_escaped_loop_W out1 out s (bs ++ [34]) a 0#usize (by simpa [B, esc] using hW1) (by simpa [B, esc] using ha)
    (by simp)
  split
  · simp
  rename_i _ a1
  simp only at o1_post
  obtain ⟨ha1, hW2⟩ := o1_post
  step with put_byte_W out2 out (bs ++ [34] ++ esc (B s.val)) a1 34#u8 hW2 (by rw [ha1]; simp; omega)
  split
  · simp
  rename_i _ a2
  simp only at r_post
  obtain ⟨ha2, hW3⟩ := r_post
  refine ⟨by rw [ha2]; simp [tokBytes]; omega, ?_⟩
  simpa [tokBytes] using hW3

/-- `put_key` writes exactly the bytes of a key token (the string and a colon) at `pos`. -/
theorem put_key_W (o out s : Slice U8) (bs : List Nat) (pos : Usize)
    (hW : W o out bs) (hp : pos.val = bs.length) :
    slice.put_key o pos s ⦃ r o' => match r with
      | none => True
      | some p => p.val = bs.length + (tokBytes (.key (B s.val))).length ∧
          W o' out (bs ++ tokBytes (.key (B s.val))) ⦄ := by
  unfold slice.put_key
  step with put_str_W o out s bs pos hW hp
  split
  · simp
  rename_i _ a
  simp only at o_post
  obtain ⟨ha, hW1⟩ := o_post
  step with put_byte_W out1 out (bs ++ tokBytes (.str (B s.val))) a 58#u8 hW1 (by rw [ha]; simp)
  split
  · simp
  rename_i _ a1
  simp only at r_post
  obtain ⟨ha1, hW2⟩ := r_post
  refine ⟨by rw [ha1]; simp [tokBytes]; omega, ?_⟩
  simpa [tokBytes] using hW2

theorem take_set_succ {α} (L : List α) (k : Nat) (x : α) (h : k < L.length) :
    (L.set k x).take (k + 1) = L.take k ++ [x] := by
  rw [List.take_succ_eq_append_getElem (by simpa using h), List.take_set_of_le (le_refl _),
    List.getElem_set_self]

theorem rdf_u64_div (m : U64) : (rdf (m.val / 10)).length ≤ 19 :=
  rdf_len _ _ (by have := m.hBounds; simp at this ⊢; omega)

theorem put_dec_loop0_W (d : Std.Array U8 20#usize) (k : Usize) (m : U64) (T : List Nat)
    (hk : k.val + (rdf m.val).length ≤ 20)
    (heq : B (d.val.take k.val) ++ (rdf m.val).map (48 + ·) = T) :
    slice.put_dec_loop0 d k m ⦃ r => r.2.val ≤ 20 ∧ B (r.1.val.take r.2.val) = T ⦄ := by
  unfold slice.put_dec_loop0
  apply loop.spec_decr_nat (measure := fun (x : Std.Array U8 20#usize × Usize × U64) => x.2.2.val)
    (inv := fun x => x.2.1.val + (rdf x.2.2.val).length ≤ 20 ∧
      B (x.1.val.take x.2.1.val) ++ (rdf x.2.2.val).map (48 + ·) = T)
  · rintro ⟨a, j, q⟩ ⟨hj, he⟩
    simp only at hj he
    unfold slice.put_dec_loop0.body
    by_cases hq : 0 < q.val
    · rw [rdf_pos q.val hq] at hj he
      simp only [List.length_cons] at hj
      have hgt : q > 0#u64 := by scalar_tac
      have hlt : j < 20#usize := by scalar_tac
      simp only [hgt, hlt, if_true]
      step*
      have hd : i1.val = q.val % 10 := by
        rw [i1_post, UScalar.cast_val_mod_pow_of_inBounds_eq _ _ (by simp; omega), i_post]
      refine ⟨?_, ?_, by scalar_tac⟩
      · rw [k1_post, m1_post]; omega
      · rw [← he, k1_post, m1_post, a_post, Std.Array.set_val_eq,
          take_set_succ _ _ _ (by simp; omega)]
        simp [B, i2_post, hd]
    · have hq0 : q.val = 0 := by omega
      have hng : ¬ q > 0#u64 := by scalar_tac
      simp only [hng, if_false, WP.spec_ok]
      rw [hq0, rdf] at he hj
      simp at he hj
      exact ⟨by omega, he⟩
  · exact ⟨hk, heq⟩

theorem rev_take_succ (d : List U8) (k i : Nat) (hk : k ≤ d.length) (hi : i < k) :
    (B (d.take k)).reverse.take (i + 1) =
      (B (d.take k)).reverse.take i ++ [(d[k - 1 - i]'(by omega)).val] := by
  have hl : (B (d.take k)).reverse.length = k := by simp [B]; omega
  rw [List.take_succ_eq_append_getElem (by rw [hl]; exact hi)]
  congr 2
  simp only [B, List.getElem_reverse, List.getElem_map, List.getElem_take, List.length_map,
    List.length_take, Nat.min_eq_left hk]

theorem put_dec_loop1_W (o out : Slice U8) (bs : List Nat) (pos : Usize) (d : Std.Array U8 20#usize)
    (k i : Usize) (hk : k.val ≤ 20) (hp : pos.val = bs.length) (hroom : bs.length + k.val ≤ out.length)
    (hi : i.val ≤ k.val) (hW : W o out (bs ++ (B (d.val.take k.val)).reverse.take i.val)) :
    slice.put_dec_loop1 o pos d k i ⦃ o' => W o' out (bs ++ (B (d.val.take k.val)).reverse) ⦄ := by
  unfold slice.put_dec_loop1
  have hdl : d.val.length = 20 := by simp
  apply loop.spec_decr_nat (measure := fun (x : Slice U8 × Usize) => k.val - x.2.val)
    (inv := fun x => x.2.val ≤ k.val ∧ W x.1 out (bs ++ (B (d.val.take k.val)).reverse.take x.2.val))
  · rintro ⟨o1, j⟩ ⟨hj, hW1⟩
    simp only at hj hW1
    unfold slice.put_dec_loop1.body
    by_cases hjk : j.val < k.val
    · have hlt : j < k := by scalar_tac
      simp only [hlt, if_true]
      have hl1 := hW1.1
      step*
      refine ⟨by scalar_tac, ?_, by scalar_tac⟩
      have e := rev_take_succ d.val k.val j.val (by omega) hjk
      rw [i5_post, e, ← List.append_assoc, s_post]
      have hidx : i2.val = k.val - 1 - j.val := by scalar_tac
      have hdi : (d.val[k.val - 1 - j.val]'(by omega)).val = i3.val := by
        rw [i3_post]; simp only [hidx]
      rw [hdi]
      have hrl : (B (d.val.take k.val)).reverse.length = k.val := by simp [B]; omega
      exact hW1.set1 i4 (by simp only [List.length_append, List.length_take, hrl]; omega)
        (by simp only [List.length_append, List.length_take, hrl]; omega) i3
    · have hge : ¬ j < k := by scalar_tac
      simp only [hge, if_false, WP.spec_ok]
      have hjk' : j.val = k.val := by omega
      have hl : (B (d.val.take k.val)).reverse.length = k.val := by simp [B]; omega
      rw [hjk', List.take_of_length_le (by omega)] at hW1
      exact hW1
  · exact ⟨hi, hW⟩

/-- `put_dec` writes exactly the bytes of a number token (`n` in decimal) at `pos`. -/
theorem put_dec_W (o out : Slice U8) (bs : List Nat) (pos : Usize) (n : U64)
    (hW : W o out bs) (hp : pos.val = bs.length) :
    slice.put_dec o pos n ⦃ r o' => match r with
      | none => True
      | some p => p.val = bs.length + (tokBytes (.num n.val)).length ∧
          W o' out (bs ++ tokBytes (.num n.val)) ⦄ := by
  unfold slice.put_dec
  have hl := hW.1
  step
  step
  step
  step
  step
  step
  have hd : i1.val = n.val % 10 := by
    rw [i1_post, UScalar.cast_val_mod_pow_of_inBounds_eq _ _ (by simp; omega), o']
  have hr := rdf_u64_div n
  step with put_dec_loop0_W a k m (((n.val % 10) :: rdf (n.val / 10)).map (48 + ·))
    (by rw [k_post, m_post]; omega)
    (by rw [k_post, m_post, a_post, Std.Array.set_val_eq]; simp [B, i2_post, hd])
  have hk1 := digits1_post
  have hD := digits1_post1
  have hk1v : k1.val = (dec n.val).length := by
    have := congrArg List.length hD
    have h20 : digits1.val.length = 20 := by simp
    simp only [B, List.length_map, List.length_take, h20, List.length_cons] at this
    rw [dec_eq]; simp only [List.length_reverse, List.length_map, List.length_cons]; omega
  split
  · simp
  step
  split
  · simp
  have hroom : bs.length + k1.val ≤ out.length := by scalar_tac
  step with put_dec_loop1_W o out bs pos digits1 k1 0#usize hk1 hp hroom (by simp) (by simpa using hW)
  step
  rw [hD, ← dec_eq] at out1_post
  exact ⟨by simp [tokBytes]; scalar_tac, by simpa [tokBytes] using out1_post⟩

/-- What `put_str` writes at `pos` is one JSON string that lexes back to `s` and
ends at its own closing quote: `s`'s bytes cannot end it early. -/
theorem put_str_contained (o out s : Slice U8) (bs : List Nat) (pos : Usize)
    (hW : W o out bs) (hp : pos.val = bs.length) :
    slice.put_str o pos s ⦃ r o' => match r with
      | none => True
      | some _ => ∃ rest, B o'.val = bs ++ 34 :: (esc (B s.val) ++ 34 :: rest) ∧
          lexStr (esc (B s.val) ++ 34 :: rest) = some (B s.val, rest) ⦄ := by
  apply WP.spec_mono (put_str_W o out s bs pos hW hp)
  rintro ⟨r, o'⟩ h
  cases r with
  | none => trivial
  | some p =>
    obtain ⟨-, -, -, hB⟩ := h
    refine ⟨(B out.val).drop (bs ++ tokBytes (.str (B s.val))).length, ?_, ?_⟩
    · rw [hB]; simp [tokBytes]
    · exact lex_esc _ _ (fun c hc => by simp only [B, List.mem_map] at hc; obtain ⟨x, -, rfl⟩ := hc; scalar_tac)

/-! Totality: no slice writer panics, for any buffer, position and input. -/

theorem put_byte_total (o : Slice U8) (pos : Usize) (b : U8) : slice.put_byte o pos b ⦃ _ _ => True ⦄ := by
  unfold slice.put_byte; step*

theorem put_escaped_byte_total (o : Slice U8) (pos : Usize) (c : U8) :
    slice.put_escaped_byte o pos c ⦃ _ _ => True ⦄ := by
  by_cases h : pos.val ≤ o.length
  · exact WP.spec_mono (put_escaped_byte_W o o _ pos c (W.start o pos.val h) (by simp [B]; omega))
      (fun _ _ => trivial)
  · unfold slice.put_escaped_byte slice.put_byte
    have : pos > o.len := by scalar_tac
    have : pos ≥ o.len := by scalar_tac
    simp only [*, if_true]
    split_ifs <;> simp

@[step] theorem put_escaped_byte_total' (o : Slice U8) (pos : Usize) (c : U8) :
    slice.put_escaped_byte o pos c ⦃ _ _ => True ⦄ := put_escaped_byte_total o pos c

theorem put_escaped_total (o : Slice U8) (pos : Usize) (s : Slice U8) :
    slice.put_escaped o pos s ⦃ _ _ => True ⦄ := by
  unfold slice.put_escaped slice.put_escaped_loop
  apply loop.spec_decr_nat (measure := fun (x : Slice U8 × Usize × Usize) => s.length - x.2.2.val)
    (inv := fun _ => True)
  · rintro ⟨o1, at1, j⟩ -
    unfold slice.put_escaped_loop.body
    step*
  · trivial

@[step] theorem put_escaped_total' (o : Slice U8) (pos : Usize) (s : Slice U8) :
    slice.put_escaped o pos s ⦃ _ _ => True ⦄ := put_escaped_total o pos s

@[step] theorem put_byte_total' (o : Slice U8) (pos : Usize) (b : U8) :
    slice.put_byte o pos b ⦃ _ _ => True ⦄ := put_byte_total o pos b

theorem put_str_total (o : Slice U8) (pos : Usize) (s : Slice U8) : slice.put_str o pos s ⦃ _ _ => True ⦄ := by
  unfold slice.put_str
  step with put_byte_total
  split
  · simp
  step with put_escaped_total
  split
  · simp
  exact put_byte_total _ _ _

theorem put_key_total (o : Slice U8) (pos : Usize) (s : Slice U8) : slice.put_key o pos s ⦃ _ _ => True ⦄ := by
  unfold slice.put_key
  step with put_str_total
  split
  · simp
  exact put_byte_total _ _ _

theorem put_dec_total (o : Slice U8) (pos : Usize) (n : U64) : slice.put_dec o pos n ⦃ _ _ => True ⦄ := by
  by_cases h : pos.val ≤ o.length
  · exact WP.spec_mono (put_dec_W o o _ pos n (W.start o pos.val h) (by simp [B]; omega)) (fun _ _ => trivial)
  · unfold slice.put_dec
    step as ⟨x, hx⟩
    step
    step
    step
    step
    step
    have hr := rdf_u64_div n
    step with put_dec_loop0_W a k m (((n.val % 10) :: rdf (n.val / 10)).map (48 + ·))
      (by rw [k_post, m_post]; omega)
      (by rw [k_post, m_post, a_post, Std.Array.set_val_eq]; simp [B]; rw [i2_post, i1_post, UScalar.cast_val_mod_pow_of_inBounds_eq _ _ (by simp; omega), hx])
    have : pos > o.len := by scalar_tac
    simp only [this, if_true, WP.spec_ok]
    simp

end h5i_app_json.Proofs
