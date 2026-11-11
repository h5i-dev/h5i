import H1Proofs
/-!
# Response heads with extra fields

`write_head_with` writes a whole response head: `write_head_open`, then the
first `n` extra fields through `write_field` (`write_fields`), then
`end_head`. `write_head_with_spec`: on `Some(p)`, every field written passed
`write_field`'s checks and `out` starts with exactly `Spec.renderHeadWith`
of those fields, `p` bytes long.
-/

open Aeneas Aeneas.Std Result h5i_app_h1

namespace h5i_app_h1.Proofs

/-- A field as bytes. -/
def fview (f : Slice U8 × Slice U8) : List Nat × List Nat := (B f.1.val, B f.2.val)

/-- What `write_field` checks: a non-empty token name that is not reserved,
and a value of field-value bytes without SP or HTAB at either end. -/
def FieldOk (f : Slice U8 × Slice U8) : Prop :=
  0 < f.1.length ∧ Spec.allIn Spec.isTchar (G f.1) 0 f.1.length = true ∧
    reservedSpec (G f.1) f.1.length = false ∧
    Spec.allIn Spec.isFieldByte (G f.2) 0 f.2.length = true ∧
    (0 < f.2.length → Spec.isOws (G f.2 0) = false ∧ Spec.isOws (G f.2 (f.2.length - 1)) = false)

/-- The bytes `field_if` writes. -/
def fieldBytes (f : Slice U8 × Slice U8) (cond : Bool) : List Nat :=
  if cond then Spec.renderField (fview f) else []

theorem field_if_spec (o out : Slice U8) (bs : List Nat) (pos : Usize) (f : Slice U8 × Slice U8) (cond : Bool)
    (hW : W o out bs) (hp : pos.val = bs.length) :
    field_if o pos f cond ⦃ r o' => match r with
      | none => True
      | some p => (cond = true → FieldOk f) ∧ p.val = (bs ++ fieldBytes f cond).length ∧
          W o' out (bs ++ fieldBytes f cond) ⦄ := by
  obtain ⟨nm, v⟩ := f
  rw [show field_if o pos (nm, v) cond = (if cond then write_field o pos nm v else ok (some pos, o)) from rfl]
  split
  · rename_i hc
    apply WP.spec_mono (write_field_spec o out bs pos nm v hW hp)
    rintro ⟨r, o'⟩ h
    cases r with
    | none => trivial
    | some p =>
      obtain ⟨h1, h2, h3, h4, h5, h6, h7⟩ := h
      have hb : fieldBytes (nm, v) cond = B nm.val ++ [58, 32] ++ B v.val ++ [13, 10] := by
        simp [fieldBytes, hc, Spec.renderField, fview]
      refine ⟨fun _ => ⟨h1, h2, h3, h4, h5⟩, ?_, ?_⟩
      · rw [hb, h6]; simp [B, Slice.length]; omega
      · rw [hb]; simpa [List.append_assoc] using h7
  · rename_i hc
    simp only [WP.spec_ok]
    have hb : fieldBytes (nm, v) cond = [] := by simp [fieldBytes, hc]
    rw [hb, List.append_nil]
    exact ⟨fun h => absurd h hc, hp, hW⟩

/-- The bytes of the first `n` fields. -/
def fieldsBytes (fs : List (Slice U8 × Slice U8)) (n : Nat) : List Nat :=
  ((fs.take n).map fview).map Spec.renderField |>.flatten

theorem fieldsBytes_four (a b c d : Slice U8 × Slice U8) (n : Nat) :
    fieldsBytes [a, b, c, d] n =
      fieldBytes a (decide (0 < n)) ++ fieldBytes b (decide (1 < n)) ++ fieldBytes c (decide (2 < n)) ++
        fieldBytes d (decide (3 < n)) := by
  obtain rfl | rfl | rfl | rfl | h : n = 0 ∨ n = 1 ∨ n = 2 ∨ n = 3 ∨ 4 ≤ n := by omega
  · simp [fieldsBytes, fieldBytes]
  · simp [fieldsBytes, fieldBytes]
  · simp [fieldsBytes, fieldBytes]
  · simp [fieldsBytes, fieldBytes]
  · rw [fieldsBytes, List.take_of_length_le (by simp; omega)]
    simp [fieldBytes, show 0 < n by omega, show 1 < n by omega, show 2 < n by omega, show 3 < n by omega]

/-- `write_fields`: on `Some(p)`, each of the first `n` fields (all four
if `n > 4`) passed `write_field`'s checks, and exactly their bytes were
written at `pos`, in order. -/
theorem write_fields_spec (o out : Slice U8) (bs : List Nat) (pos : Usize)
    (fs : Array (Slice U8 × Slice U8) 4#usize) (n : Usize) (hW : W o out bs) (hp : pos.val = bs.length) :
    write_fields o pos fs n ⦃ r o' => match r with
      | none => True
      | some p => (∀ f ∈ fs.val.take n.val, FieldOk f) ∧
          p.val = (bs ++ fieldsBytes fs.val n.val).length ∧ W o' out (bs ++ fieldsBytes fs.val n.val) ⦄ := by
  unfold write_fields
  have hlen : fs.val.length = 4 := by rw [fs.property]; rfl
  have h4 : ∃ a b c d, fs.val = [a, b, c, d] := by
    generalize fs.val = l at hlen ⊢
    match l, hlen with
    | [a, b, c, d], _ => exact ⟨a, b, c, d, rfl⟩
  obtain ⟨a, b, c, d, hl⟩ := h4
  step as ⟨x0, y0, e0⟩
  step as ⟨x1, y1, e1⟩
  step as ⟨x2, y2, e2⟩
  step as ⟨x3, y3, e3⟩
  simp only [hl, List.getElem_cons_zero, List.getElem_cons_succ] at e0 e1 e2 e3
  subst e0 e1 e2 e3
  rw [hl, fieldsBytes_four]
  have c0 : decide (n > 0#usize) = decide (0 < n.val) := by simp only [decide_eq_decide]; scalar_tac
  have c1 : decide (n > 1#usize) = decide (1 < n.val) := by simp only [decide_eq_decide]; scalar_tac
  have c2 : decide (n > 2#usize) = decide (2 < n.val) := by simp only [decide_eq_decide]; scalar_tac
  have c3 : decide (n > 3#usize) = decide (3 < n.val) := by simp only [decide_eq_decide]; scalar_tac
  rw [c0, c1, c2, c3]
  step with field_if_spec o out bs pos _ _ hW hp
  cases o with
  | none => simp
  | some p0 =>
  obtain ⟨f0, hp0, hW0⟩ := o_post
  simp only
  step with field_if_spec out1 out _ p0 _ _ hW0 hp0
  cases o1 with
  | none => simp
  | some p1 =>
  obtain ⟨f1, hp1, hW1⟩ := o1_post
  simp only
  step with field_if_spec out2 out _ p1 _ _ hW1 hp1
  cases o2 with
  | none => simp
  | some p2 =>
  obtain ⟨f2, hp2, hW2⟩ := o2_post
  simp only
  apply WP.spec_mono (field_if_spec out3 out _ p2 _ _ hW2 hp2)
  rintro ⟨r, o'⟩ h
  cases r with
  | none => trivial
  | some p3 =>
  obtain ⟨f3, hp3, hW3⟩ := h
  refine ⟨?_, by rw [hp3]; simp [List.append_assoc], by simpa [List.append_assoc] using hW3⟩
  intro f hf
  obtain h | h | h | h | h : n.val = 0 ∨ n.val = 1 ∨ n.val = 2 ∨ n.val = 3 ∨ 4 ≤ n.val := by omega
  rotate_left 4
  · rw [List.take_of_length_le (by simp; omega)] at hf
    simp only [List.mem_cons, List.not_mem_nil, or_false] at hf
    rcases hf with rfl | rfl | rfl | rfl
    · exact f0 (by simp; omega)
    · exact f1 (by simp; omega)
    · exact f2 (by simp; omega)
    · exact f3 (by simp; omega)
  all_goals
    rw [h] at hf
    simp only [List.take_succ_cons, List.take_zero, List.mem_cons, List.not_mem_nil, or_false] at hf
    try rcases hf with rfl | rfl | rfl | rfl <;> first
      | exact f0 (by simp; omega) | exact f1 (by simp; omega) | exact f2 (by simp; omega)

/-- `write_head_with`: on `Some(p)`, the status is in 100..=599, the
content type is field-value bytes, each of the first `n` fields (all four
if `n > 4`) passed `write_field`'s checks, and `out` starts with exactly
`Spec.renderHeadWith` of them, `p` bytes long; the rest of `out` is
unchanged. -/
theorem write_head_with_spec (out : Slice U8) (status : U16) (ct : Slice U8) (bl : U32) (close : Bool)
    (fs : Array (Slice U8 × Slice U8) 4#usize) (n : Usize) :
    write_head_with out status ct bl close fs n ⦃ r o => match r with
      | none => True
      | some p => 100 ≤ status.val ∧ status.val ≤ 599 ∧
          Spec.allIn Spec.isFieldByte (G ct) 0 ct.length = true ∧
          (∀ f ∈ fs.val.take n.val, FieldOk f) ∧
          p.val = (Spec.renderHeadWith status.val (B ct.val) bl.val close ((fs.val.take n.val).map fview)).length ∧
          W o out (Spec.renderHeadWith status.val (B ct.val) bl.val close ((fs.val.take n.val).map fview)) ⦄ := by
  unfold write_head_with
  step with write_head_open_spec
  cases o with
  | none => simp
  | some a =>
  obtain ⟨h1, h2, h3, ha, hW⟩ := o_post
  simp only
  step with write_fields_spec out1 out _ a fs n hW ha
  cases o1 with
  | none => simp
  | some b =>
  obtain ⟨h5, hb, hW2⟩ := o1_post
  simp only
  apply WP.spec_mono (end_head_W out2 out _ b hW2 hb)
  rintro ⟨r, o'⟩ h
  have hr : Spec.renderHeadWith status.val (B ct.val) bl.val close ((fs.val.take n.val).map fview) =
      Spec.renderOpen status.val (B ct.val) bl.val close ++ fieldsBytes fs.val n.val ++ [13, 10] := by
    simp [Spec.renderHeadWith, fieldsBytes]
  cases r with
  | none => trivial
  | some p => rw [hr]; exact ⟨h1, h2, h3, h5, h.1, h.2⟩

/-- `write_head_with` never panics. -/
theorem write_head_with_total (out : Slice U8) (status : U16) (ct : Slice U8) (bl : U32) (close : Bool)
    (fs : Array (Slice U8 × Slice U8) 4#usize) (n : Usize) :
    write_head_with out status ct bl close fs n ⦃ _ _ => True ⦄ :=
  WP.spec_mono (write_head_with_spec out status ct bl close fs n) (fun _ _ => trivial)

end h5i_app_h1.Proofs
