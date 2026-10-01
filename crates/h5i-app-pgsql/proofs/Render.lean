import Abs
/-!
# The extracted printer prints `Pg.render`

`render_spec`: on a `Small` statement, `render` returns `H5iAppLib.Pg.render` of it.
Each writer's post is `B r = B out ++ text`; loops share `loop_append`.
-/
open Aeneas Aeneas.Std Result H5iAppLib

namespace h5i_app_pgsql

theorem B_len (v : alloc.vec.Vec U8) : (B v).length = v.length := by simp [B]

/-- The text a loop appends over indices `i..n`. -/
def seg (g : Nat → List Nat) (i n : Nat) : List Nat := (List.range' i (n - i)).flatMap g

theorem seg_self (g : Nat → List Nat) (i : Nat) : seg g i i = [] := by simp [seg]

theorem seg_succ (g : Nat → List Nat) (i j : Nat) (h : i ≤ j) : seg g i (j + 1) = seg g i j ++ g j := by
  simp only [seg]
  rw [show j + 1 - i = (j - i) + 1 by omega, List.range'_concat, List.flatMap_append]
  simp [show i + (j - i) = j by omega]

theorem seg_len_le (g : Nat → List Nat) (i j n : Nat) (hij : i ≤ j) (hjn : j ≤ n) :
    (seg g i j).length ≤ (seg g i n).length := by
  induction n with
  | zero => have : j = 0 := by omega
            subst this; simp
  | succ n ih =>
    by_cases h : j = n + 1
    · subst h; exact le_refl _
    · have := ih (by omega)
      rw [seg_succ g i n (by omega)]; simp; omega

/-- A loop over `(out, j)` that appends `g j` at each `j < n`. -/
def AppendStep (n : Nat) (g : Nat → List Nat) (o : alloc.vec.Vec U8) (j : Usize) :
    ControlFlow (alloc.vec.Vec U8 × Usize) (alloc.vec.Vec U8) → Prop
  | .done r => n ≤ j.val ∧ r = o
  | .cont (o', j') => j.val < n ∧ j'.val = j.val + 1 ∧ B o' = B o ++ g j.val

theorem loop_append (n : Nat) (g : Nat → List Nat)
    (body : alloc.vec.Vec U8 × Usize → Result (ControlFlow (alloc.vec.Vec U8 × Usize) (alloc.vec.Vec U8)))
    (hstep : ∀ (o : alloc.vec.Vec U8) (j : Usize), j.val ≤ n →
      (j.val < n → o.length + (g j.val).length ≤ Usize.max) → body (o, j) ⦃ AppendStep n g o j ⦄)
    (out : alloc.vec.Vec U8) (i : Usize) (hi : i.val ≤ n)
    (hb : out.length + (seg g i.val n).length ≤ Usize.max) :
    loop body (out, i) ⦃ r => B r = B out ++ seg g i.val n ⦄ := by
  apply loop.spec_decr_nat (measure := fun (p : alloc.vec.Vec U8 × Usize) => n - p.2.val)
    (inv := fun p => i.val ≤ p.2.val ∧ p.2.val ≤ n ∧ B p.1 = B out ++ seg g i.val p.2.val)
  · rintro ⟨o, j⟩ ⟨hij, hjn, heq⟩
    simp only at hij hjn heq
    have hol : o.length = out.length + (seg g i.val j.val).length := by
      rw [← B_len, heq, List.length_append, B_len]
    apply WP.spec_mono (hstep o j hjn (fun hlt => by
      have h1 := seg_len_le g i.val (j.val + 1) n (by omega) (by omega)
      rw [seg_succ g _ _ hij, List.length_append] at h1
      omega))
    intro r hr
    cases r with
    | done r =>
      obtain ⟨hn, rfl⟩ := hr
      simp only
      rw [heq, show j.val = n by omega]
    | cont x =>
      obtain ⟨o', j'⟩ := x
      obtain ⟨hlt, hj', ho'⟩ := hr
      simp only
      refine ⟨⟨by omega, by omega, ?_⟩, by omega⟩
      rw [ho', heq, hj', seg_succ g _ _ hij, List.append_assoc]
  · exact ⟨le_refl _, hi, by simp [seg_self]⟩

/-- Loop text over a list, when each step depends on the prefix only by length. -/
theorem seg_list {α : Type} (l : List α) (f : Nat → α → List Nat) (F : List α → List Nat)
    (h0 : F [] = []) (hs : ∀ xs x, F (xs ++ [x]) = F xs ++ f xs.length x) :
    seg (fun j => (l[j]?.map (f j)).getD []) 0 l.length = F l := by
  suffices ∀ m, m ≤ l.length → seg (fun j => (l[j]?.map (f j)).getD []) 0 m = F (l.take m) by
    simpa using this l.length (le_refl _)
  intro m hm
  induction m with
  | zero => simp [seg_self, h0]
  | succ m ih =>
    rw [seg_succ _ _ _ (by omega), ih (by omega), List.take_succ_eq_append_getElem (by omega), hs]
    simp [List.getElem?_eq_getElem (show m < l.length by omega), Nat.min_eq_left (show m ≤ l.length by omega)]

theorem commaSep_snoc (xs : List (List Nat)) (x : List Nat) :
    Pg.commaSep (xs ++ [x]) = Pg.commaSep xs ++ ((if xs.length = 0 then [] else Pg.lit ", ") ++ x) := by
  cases xs with
  | nil => simp [Pg.commaSep]
  | cons y ys => simp [Pg.commaSep, List.flatMap_append]

theorem whereText_snoc (cs : List Pg.Cond) (c : Pg.Cond) :
    Pg.whereText (cs ++ [c]) =
      Pg.whereText cs ++ ((if cs.length = 0 then Pg.lit " WHERE " else Pg.lit " AND ") ++ Pg.condText c) := by
  cases cs with
  | nil => simp [Pg.whereText]
  | cons y ys => simp [Pg.whereText, List.flatMap_append]

/-- A slice's bytes. -/
def SB (s : Slice U8) : List Nat := s.val.map (·.val)

theorem getD_map_some {α : Type} (l : List α) (f : α → List Nat) (j : Nat) (h : j < l.length) :
    (l[j]?.map f).getD [] = f l[j] := by
  simp [List.getElem?_eq_getElem h]

@[step]
theorem push_lit_spec (out : alloc.vec.Vec U8) (s : Slice U8) (h : out.length + (SB s).length ≤ Usize.max) :
    push_lit out s ⦃ r => B r = B out ++ SB s ⦄ := by
  have e := seg_list (SB s) (fun _ x => [x]) id rfl (fun _ _ => rfl)
  simp only [id] at e
  unfold push_lit push_lit_loop
  have hl : (SB s).length = s.length := by simp [SB]
  rw [← e]
  have h0 : (0#usize : Usize).val = 0 := rfl
  apply loop_append (SB s).length _ _ _ out 0#usize (by simp) (by rw [h0, e]; exact h)
  intro o j hj hb
  dsimp only
  unfold push_lit_loop.body
  dsimp only
  split
  · rename_i hlt
    have hlt' : j.val < (SB s).length := by rw [hl]; scalar_tac
    have hb' := hb hlt'
    rw [getD_map_some _ _ _ hlt'] at hb'
    step*
    refine ⟨hlt', by scalar_tac, ?_⟩
    simp only
    rw [getD_map_some _ _ _ hlt']
    simp [B, out1_post, i2_post, SB]
  · simp only [WP.spec_ok, AppendStep]
    refine ⟨by rw [hl]; scalar_tac, trivial⟩

/-- One byte of a quoted name. -/
def esc (c : Nat) : List Nat := if c = 34 then [34, 34] else [c]

theorem quote_eq (n : List Nat) : Pg.quote n = 34 :: n.flatMap esc ++ [34] := rfl

@[step]
theorem push_name_loop_spec (out n : alloc.vec.Vec U8)
    (h : out.length + ((B n).flatMap esc).length ≤ Usize.max) :
    push_name_loop out n 0#usize ⦃ r => B r = B out ++ (B n).flatMap esc ⦄ := by
  have e := seg_list (B n) (fun _ x => esc x) (fun l => l.flatMap esc) rfl (fun _ _ => by simp)
  have hl : (B n).length = n.length := B_len n
  have h0 : (0#usize : Usize).val = 0 := rfl
  unfold push_name_loop
  rw [← e]
  apply loop_append (B n).length _ _ _ out 0#usize (by simp) (by rw [h0, e]; exact h)
  intro o j hj hb
  dsimp only
  unfold push_name_loop.body
  dsimp only
  split
  · rename_i hlt
    have hlt' : j.val < (B n).length := by rw [hl]; scalar_tac
    have hb' := hb hlt'
    rw [getD_map_some _ _ _ hlt'] at hb'
    have hbj : (B n)[j.val] = (n.val[j.val]'(by scalar_tac)).val := by simp [B]
    by_cases hq : (n.val[j.val]'(by scalar_tac)) = 34#u8
    · have hv : (B n)[j.val] = 34 := by rw [hbj, hq]; rfl
      rw [hv] at hb'
      simp [esc] at hb'
      step*
      simp only [i2_post, hq, if_true]
      step*
      all_goals try (simp_all; scalar_tac)
      refine ⟨hlt', by scalar_tac, ?_⟩
      simp only
      rw [getD_map_some _ _ _ hlt', hv]
      simp [B, out2_post, out1_post, esc]
    · have hv : (B n)[j.val] ≠ 34 := by
        rw [hbj]; intro h'; apply hq; exact (u8_eq_iff _ _).2 (by simpa using h')
      simp [esc, hv] at hb'
      step*
      simp only [i2_post, hq, if_false]
      step*
      refine ⟨hlt', by scalar_tac, ?_⟩
      simp only
      rw [getD_map_some _ _ _ hlt']
      simp [B, out2_post, esc]
      rw [← hbj]; exact hv
  · simp only [WP.spec_ok, AppendStep]
    refine ⟨by rw [hl]; scalar_tac, trivial⟩


theorem len_of_B {v w : alloc.vec.Vec U8} {l : List Nat} (h : B v = B w ++ l) : v.length = w.length + l.length := by
  rw [← B_len, ← B_len, h, List.length_append]

theorem B_push (v : alloc.vec.Vec U8) (l : List U8) (w : alloc.vec.Vec U8) (h : w.val = v.val ++ l) :
    B w = B v ++ l.map (·.val) := by
  simp [B, h]

@[step]
theorem push_name_spec (out n : alloc.vec.Vec U8) (h : out.length + (Pg.quote (B n)).length ≤ Usize.max) :
    push_name out n ⦃ r => B r = B out ++ Pg.quote (B n) ⦄ := by
  rw [quote_eq] at h ⊢
  simp only [List.length_cons, List.length_append, List.length_nil, alloc.vec.Vec.length] at h
  unfold push_name
  step
  have h1 := B_push _ _ _ out1_post
  have h1' := len_of_B h1
  simp only [List.length_map, List.length_singleton, alloc.vec.Vec.length] at h1'
  step with push_name_loop_spec out1 n (by simp only [alloc.vec.Vec.length]; omega)
  have h2 := len_of_B out2_post
  simp only [alloc.vec.Vec.length] at h2
  step
  have h3 := B_push _ _ _ r_post
  rw [h3, out2_post, h1]
  simp

theorem digits_len_pos (n : Nat) : 1 ≤ (Pg.digits n).length := by
  rw [Pg.digits]; split <;> simp

@[step]
theorem push_dec_spec (out : alloc.vec.Vec U8) (n : U32) (h : out.length + (Pg.digits n.val).length ≤ Usize.max) :
    push_dec out n ⦃ r => B r = B out ++ Pg.digits n.val ⦄ := by
  induction hn : n.val using Nat.strong_induction_on generalizing n out with
  | _ m ih =>
    subst hn
    rw [push_dec]
    by_cases h10 : n.val < 10
    · have e : Pg.digits n.val = [48 + n.val] := by rw [Pg.digits, if_pos h10]
      rw [e] at h ⊢
      simp only [List.length_singleton, alloc.vec.Vec.length] at h
      have : ¬ (n ≥ 10#u32) := by scalar_tac
      simp only [this, if_false]
      step*
      have hi1 : i1.val = n.val := by
        rw [i1_post, UScalar.cast_val_mod_pow_of_inBounds_eq _ _ (by simp; scalar_tac), i_post]; omega
      rw [B_push _ _ _ r_post]
      simp [i2_post, hi1]
    · have e : Pg.digits n.val = Pg.digits (n.val / 10) ++ [48 + n.val % 10] := by rw [Pg.digits, if_neg h10]
      rw [e] at h ⊢
      simp only [List.length_append, List.length_singleton, alloc.vec.Vec.length] at h
      have : n ≥ 10#u32 := by scalar_tac
      simp only [this, if_true]
      step as ⟨ q, hq ⟩
      step with ih q.val (by scalar_tac) out q (by simp only [alloc.vec.Vec.length]; rw [hq]; omega) rfl as ⟨ o1, ho1 ⟩
      have hl := len_of_B ho1
      simp only [alloc.vec.Vec.length] at hl
      step*
      have hi1 : i1.val = n.val % 10 := by
        rw [i1_post, UScalar.cast_val_mod_pow_of_inBounds_eq _ _ (by simp; scalar_tac), i_post]
      rw [B_push _ _ _ r_post, ho1, hq]
      simp [i2_post, hi1]

@[step]
theorem push_param_spec (out : alloc.vec.Vec U8) (p : U32) (h : out.length + (Pg.param p.val).length ≤ Usize.max) :
    push_param out p ⦃ r => B r = B out ++ Pg.param p.val ⦄ := by
  simp only [Pg.param, List.length_cons, alloc.vec.Vec.length] at h
  unfold push_param
  step
  have h1 := B_push _ _ _ out1_post
  have h1' := len_of_B h1
  simp only [List.length_map, List.length_singleton, alloc.vec.Vec.length] at h1'
  step with push_dec_spec out1 p (by simp only [alloc.vec.Vec.length]; omega)
  rw [r_post, h1]
  simp [Pg.param]

theorem B_len' (v : alloc.vec.Vec U8) : v.val.length = (B v).length := by simp [B]

theorem B_push' {v w : alloc.vec.Vec U8} {l : List U8} (h : w.val = v.val ++ l) :
    B w = B v ++ l.map (·.val) := by
  simp [B, h]

open Lean Elab Tactic Meta in
/-- Restate every `↑w = ↑v ++ l` on byte vectors (a `Vec.push` post) as `B w = B v ++ _`. -/
elab "bpush" : tactic => withMainContext do
  for d in (← getLCtx) do
    if d.isImplementationDetail then continue
    try
      let e ← mkAppM ``B_push' #[d.toExpr]
      let t ← inferType e
      liftMetaTactic fun g => do
        let (_, g') ← (← g.assert (← mkFreshUserName `hb) t e).intro1P
        pure [g']
    catch _ => pure ()

/-- Writer side goals: rewrite each `B x` through the posts, compare lengths or bytes. -/
macro "emit" " [" ls:Lean.Parser.Tactic.simpLemma,* "]" : tactic => `(tactic| (
  bpush
  try simp only [alloc.vec.Vec.length] at *
  try simp only [B_len'] at *
  try simp only [*, SB, Array.to_slice, Array.make, Pg.lit, List.length_append, List.length_map,
    List.length_cons, List.length_nil, List.append_assoc, $ls,*] at *
  try simp [$ls,*] at *
  try omega))

macro "emit" : tactic => `(tactic| emit [])

@[step]
theorem push_cond_spec (out : alloc.vec.Vec U8) (c : Cond) (h : out.length + (Pg.condText c.abs).length ≤ Usize.max) :
    push_cond out c ⦃ r => B r = B out ++ Pg.condText c.abs ⦄ := by
  unfold push_cond
  cases c with
  | Eq col p =>
    simp only [Cond.abs, Pg.condText, List.length_append, alloc.vec.Vec.length] at h
    step* <;> emit [Cond.abs, Pg.condText]
  | Same col p =>
    simp only [Cond.abs, Pg.condText, List.length_append, alloc.vec.Vec.length] at h
    step* <;> emit [Cond.abs, Pg.condText]

@[step]
theorem push_kind_spec (out : alloc.vec.Vec U8) (k : Kind) (h : out.length + (Pg.kindText k.abs).length ≤ Usize.max) :
    push_kind out k ⦃ r => B r = B out ++ Pg.kindText k.abs ⦄ := by
  unfold push_kind
  cases k <;> simp only [Kind.abs, Pg.kindText] at h ⊢ <;> step* <;> emit

theorem Bs_len (ns : alloc.vec.Vec (alloc.vec.Vec U8)) : (Bs ns).length = ns.length := by simp [Bs]

theorem Bs_get (ns : alloc.vec.Vec (alloc.vec.Vec U8)) (j : Nat) (h : j < (Bs ns).length) :
    (Bs ns)[j] = B (ns.val[j]'(by simpa [Bs] using h)) := by simp [Bs]

theorem commaSep_quote (l : List Pg.Name) :
    seg (fun j => (l[j]?.map (fun x => (if j = 0 then [] else Pg.lit ", ") ++ Pg.quote x)).getD []) 0 l.length =
      Pg.commaSep (l.map Pg.quote) :=
  seg_list l (fun j x => (if j = 0 then [] else Pg.lit ", ") ++ Pg.quote x) (fun l => Pg.commaSep (l.map Pg.quote)) rfl
    (fun xs x => by rw [List.map_append, List.map_singleton, commaSep_snoc]; simp)

@[step]
theorem push_names_spec (out : alloc.vec.Vec U8) (ns : alloc.vec.Vec (alloc.vec.Vec U8))
    (h : out.length + (Pg.commaSep ((Bs ns).map Pg.quote)).length ≤ Usize.max) :
    push_names out ns ⦃ r => B r = B out ++ Pg.commaSep ((Bs ns).map Pg.quote) ⦄ := by
  have hl := Bs_len ns
  have h0 : (0#usize : Usize).val = 0 := rfl
  unfold push_names push_names_loop
  rw [← commaSep_quote]
  apply loop_append (Bs ns).length _ _ _ out 0#usize (by simp) (by rw [h0, commaSep_quote]; exact h)
  intro o j hj hb
  dsimp only
  unfold push_names_loop.body
  dsimp only
  split
  · rename_i hlt
    have hlt' : j.val < (Bs ns).length := by rw [hl]; scalar_tac
    have hb' := hb hlt'
    rw [getD_map_some _ _ _ hlt', Bs_get _ _ hlt'] at hb'
    by_cases hj0 : j.val = 0
    · have : ¬ (j > 0#usize) := by scalar_tac
      simp only [this, if_false]
      simp only [hj0, if_true, List.nil_append] at hb'
      step*
      refine ⟨hlt', by scalar_tac, ?_⟩
      simp only
      rw [getD_map_some _ _ _ hlt', Bs_get _ _ hlt']
      simp only [hj0, if_true, List.nil_append]
      emit
    · have : j > 0#usize := by scalar_tac
      simp only [this, if_true]
      simp only [hj0, if_false] at hb'
      step*
      all_goals try (emit; done)
      refine ⟨hlt', by scalar_tac, ?_⟩
      simp only
      rw [getD_map_some _ _ _ hlt', Bs_get _ _ hlt']
      simp only [hj0, if_false]
      emit
  · simp only [WP.spec_ok, AppendStep]
    refine ⟨by rw [hl]; scalar_tac, trivial⟩

theorem conds_get (cs : alloc.vec.Vec Cond) (j : Nat) (h : j < (cs.val.map Cond.abs).length) :
    (cs.val.map Cond.abs)[j] = (cs.val[j]'(by simpa using h)).abs := by simp

theorem where_seg (l : List Pg.Cond) :
    seg (fun j => (l[j]?.map (fun c => (if j = 0 then Pg.lit " WHERE " else Pg.lit " AND ") ++ Pg.condText c)).getD [])
      0 l.length = Pg.whereText l :=
  seg_list l _ Pg.whereText rfl (fun xs x => by rw [whereText_snoc])

@[step]
theorem push_where_spec (out : alloc.vec.Vec U8) (cs : alloc.vec.Vec Cond)
    (h : out.length + (Pg.whereText (cs.val.map Cond.abs)).length ≤ Usize.max) :
    push_where out cs ⦃ r => B r = B out ++ Pg.whereText (cs.val.map Cond.abs) ⦄ := by
  have hl : (cs.val.map Cond.abs).length = cs.length := by simp
  have h0 : (0#usize : Usize).val = 0 := rfl
  unfold push_where push_where_loop
  rw [← where_seg]
  apply loop_append (cs.val.map Cond.abs).length _ _ _ out 0#usize (by simp) (by rw [h0, where_seg]; exact h)
  intro o j hj hb
  dsimp only
  unfold push_where_loop.body
  dsimp only
  split
  · rename_i hlt
    have hlt' : j.val < (cs.val.map Cond.abs).length := by rw [hl]; scalar_tac
    have hb' := hb hlt'
    rw [getD_map_some _ _ _ hlt', conds_get _ _ hlt'] at hb'
    by_cases hj0 : j.val = 0
    · have : j = 0#usize := by scalar_tac
      simp only [eq_true this, if_true]
      simp only [hj0, if_true] at hb'
      step*
      all_goals try (emit; done)
      refine ⟨hlt', by scalar_tac, ?_⟩
      simp only
      rw [getD_map_some _ _ _ hlt', conds_get _ _ hlt']
      simp only [hj0, if_true]
      emit
    · have : ¬ j = 0#usize := by scalar_tac
      simp only [eq_false this, if_false]
      simp only [hj0, if_false] at hb'
      step*
      all_goals try (emit; done)
      refine ⟨hlt', by scalar_tac, ?_⟩
      simp only
      rw [getD_map_some _ _ _ hlt', conds_get _ _ hlt']
      simp only [hj0, if_false]
      emit
  · simp only [WP.spec_ok, AppendStep]
    refine ⟨by rw [hl]; scalar_tac, trivial⟩

theorem coldefs_get (cs : alloc.vec.Vec ColDef) (j : Nat) (h : j < (cs.val.map ColDef.abs).length) :
    (cs.val.map ColDef.abs)[j] = (cs.val[j]'(by simpa using h)).abs := by simp

@[step]
theorem push_coldefs_spec (out : alloc.vec.Vec U8) (cs : alloc.vec.Vec ColDef)
    (h : out.length + ((cs.val.map ColDef.abs).flatMap Pg.colDefText).length ≤ Usize.max) :
    push_coldefs out cs ⦃ r => B r = B out ++ (cs.val.map ColDef.abs).flatMap Pg.colDefText ⦄ := by
  have hl : (cs.val.map ColDef.abs).length = cs.length := by simp
  have h0 : (0#usize : Usize).val = 0 := rfl
  have e := seg_list (cs.val.map ColDef.abs) (fun _ c => Pg.colDefText c) (fun l => l.flatMap Pg.colDefText) rfl
    (fun _ _ => by simp)
  unfold push_coldefs push_coldefs_loop
  rw [← e]
  apply loop_append (cs.val.map ColDef.abs).length _ _ _ out 0#usize (by simp) (by rw [h0, e]; exact h)
  intro o j hj hb
  dsimp only
  unfold push_coldefs_loop.body
  dsimp only
  split
  · rename_i hlt
    have hlt' : j.val < (cs.val.map ColDef.abs).length := by rw [hl]; scalar_tac
    have hb' := hb hlt'
    rw [getD_map_some _ _ _ hlt', coldefs_get _ _ hlt'] at hb'
    simp only [Pg.colDefText, ColDef.abs, List.length_append, List.length_singleton] at hb'
    by_cases hnn : (cs.val[j.val]'(by scalar_tac)).not_null = true
    · simp only [hnn, if_true] at hb'
      step*
      all_goals try (emit; done)
      simp only [cd_post, hnn, if_true]
      step*
      all_goals try (emit; done)
      refine ⟨hlt', by scalar_tac, ?_⟩
      simp only
      rw [getD_map_some _ _ _ hlt', coldefs_get _ _ hlt']
      simp only [Pg.colDefText, ColDef.abs, hnn, if_true]
      emit
    · simp only [hnn] at hb'
      step*
      all_goals try (emit; done)
      simp only [cd_post, hnn, if_false, Bool.false_eq_true]
      step*
      all_goals try (emit; done)
      refine ⟨hlt', by scalar_tac, ?_⟩
      simp only
      rw [getD_map_some _ _ _ hlt', coldefs_get _ _ hlt']
      simp only [Pg.colDefText, ColDef.abs, hnn, if_false, Bool.false_eq_true]
      emit
  · simp only [WP.spec_ok, AppendStep]
    refine ⟨by rw [hl]; scalar_tac, trivial⟩

theorem placeholders_seg (n : Nat) :
    seg (fun j => ((List.range n)[j]?.map (fun x => (if j = 0 then [] else Pg.lit ", ") ++ Pg.param (x + 1))).getD [])
      0 (List.range n).length = Pg.placeholders n :=
  seg_list (List.range n) _ (fun l => Pg.commaSep (l.map (fun i => Pg.param (i + 1)))) rfl
    (fun xs x => by rw [List.map_append, List.map_singleton, commaSep_snoc]; simp)

@[step]
theorem push_placeholders_spec (out : alloc.vec.Vec U8) (n : Usize) (hn : n.val ≤ U32.max)
    (h : out.length + (Pg.placeholders n.val).length ≤ Usize.max) :
    push_placeholders out n ⦃ r => B r = B out ++ Pg.placeholders n.val ⦄ := by
  have hl : (List.range n.val).length = n.val := by simp
  have h0 : (0#usize : Usize).val = 0 := rfl
  unfold push_placeholders push_placeholders_loop
  rw [← placeholders_seg]
  apply loop_append (List.range n.val).length _ _ _ out 0#usize (by simp) (by rw [h0, placeholders_seg]; exact h)
  intro o j hj hb
  dsimp only
  unfold push_placeholders_loop.body
  split
  · rename_i hlt
    have hlt' : j.val < (List.range n.val).length := by rw [hl]; scalar_tac
    have hb' := hb hlt'
    rw [getD_map_some _ _ _ hlt'] at hb'
    simp only [List.getElem_range] at hb'
    have hr : (List.range n.val)[j.val] = j.val := by simp
    by_cases hj0 : j.val = 0
    · have : ¬ (j > 0#usize) := by scalar_tac
      simp only [this, if_false]
      simp only [hj0, if_true, List.nil_append] at hb'
      step*
      refine ⟨hlt', by scalar_tac, ?_⟩
      simp only
      rw [getD_map_some _ _ _ hlt', hr]
      have hi2 : i2.val = j.val + 1 := by scalar_tac
      simp only [hj0, if_true, List.nil_append]
      emit [hi2]
    · have : j > 0#usize := by scalar_tac
      simp only [this, if_true]
      simp only [hj0, if_false] at hb'
      step*
      all_goals first | (emit; done) | (have hi2 : i2.val = j.val + 1 := (by scalar_tac); emit [hi2]; done) | skip
      refine ⟨hlt', by scalar_tac, ?_⟩
      simp only
      rw [getD_map_some _ _ _ hlt', hr]
      have hi2 : i2.val = j.val + 1 := by scalar_tac
      simp only [hj0, if_false]
      emit [hi2]
  · simp only [WP.spec_ok, AppendStep]
    refine ⟨by rw [hl]; scalar_tac, trivial⟩

/-- `"n" = EXCLUDED."n"` -/
def setText (n : Pg.Name) : List Nat := Pg.quote n ++ Pg.lit " = EXCLUDED." ++ Pg.quote n

theorem setText_eq : setText = fun n => Pg.quote n ++ Pg.lit " = EXCLUDED." ++ Pg.quote n := rfl

theorem sets_seg (l : List Pg.Name) :
    seg (fun j => (l[j]?.map (fun x => (if j = 0 then [] else Pg.lit ", ") ++ setText x)).getD []) 0 l.length =
      Pg.commaSep (l.map setText) :=
  seg_list l _ (fun l => Pg.commaSep (l.map setText)) rfl
    (fun xs x => by rw [List.map_append, List.map_singleton, commaSep_snoc]; simp)

@[step]
theorem push_sets_spec (out : alloc.vec.Vec U8) (ns : alloc.vec.Vec (alloc.vec.Vec U8))
    (h : out.length + (Pg.commaSep ((Bs ns).map setText)).length ≤ Usize.max) :
    push_sets out ns ⦃ r => B r = B out ++ Pg.commaSep ((Bs ns).map setText) ⦄ := by
  have hl := Bs_len ns
  have h0 : (0#usize : Usize).val = 0 := rfl
  unfold push_sets push_sets_loop
  rw [← sets_seg]
  apply loop_append (Bs ns).length _ _ _ out 0#usize (by simp) (by rw [h0, sets_seg]; exact h)
  intro o j hj hb
  dsimp only
  unfold push_sets_loop.body
  dsimp only
  split
  · rename_i hlt
    have hlt' : j.val < (Bs ns).length := by rw [hl]; scalar_tac
    have hb' := hb hlt'
    rw [getD_map_some _ _ _ hlt', Bs_get _ _ hlt'] at hb'
    simp only [setText, List.length_append] at hb'
    by_cases hj0 : j.val = 0
    · have : ¬ (j > 0#usize) := by scalar_tac
      simp only [this, if_false]
      simp only [hj0, if_true] at hb'
      step*
      all_goals try (emit; done)
      refine ⟨hlt', by scalar_tac, ?_⟩
      simp only
      rw [getD_map_some _ _ _ hlt', Bs_get _ _ hlt']
      simp only [hj0, if_true, List.nil_append, setText]
      emit
    · have : j > 0#usize := by scalar_tac
      simp only [this, if_true]
      simp only [hj0, if_false] at hb'
      step*
      all_goals try (emit; done)
      refine ⟨hlt', by scalar_tac, ?_⟩
      simp only
      rw [getD_map_some _ _ _ hlt', Bs_get _ _ hlt']
      simp only [hj0, if_false, setText]
      emit
  · simp only [WP.spec_ok, AppendStep]
    refine ⟨by rw [hl]; scalar_tac, trivial⟩

/-- Text fits in memory; an `INSERT` has fewer columns than `u32` placeholders. -/
def Small (s : Sql) : Prop :=
  (Pg.render s.abs).length ≤ Usize.max ∧
    match s with
    | .Insert _ cols _ _ => cols.length ≤ U32.max
    | _ => True

theorem Bs_nil (ns : alloc.vec.Vec (alloc.vec.Vec U8)) : Bs ns = [] ↔ ns.length = 0 := by
  simp [Bs, alloc.vec.Vec.length]

/-- The extracted printer prints `Pg.render`. -/
theorem render_spec (s : Sql) (h : Small s) : render s ⦃ b => B b = Pg.render s.abs ⦄ := by
  obtain ⟨h, hs⟩ := h
  unfold render
  have hnil : B (alloc.vec.Vec.new U8) = [] := rfl
  cases s with
  | Create t cols key =>
    simp only [Sql.abs, Pg.render, List.length_append] at h
    step* <;> emit [Sql.abs, Pg.render, hnil]
  | Select t cols conds order =>
    simp only [Sql.abs, Pg.render, List.length_append] at h
    by_cases ho : order.length = 0
    · have e : Bs order = [] := (Bs_nil order).2 ho
      simp only [e, if_true, List.length_nil] at h
      step*
      all_goals first | (emit [Sql.abs, Pg.render, hnil, e])
    · have e : ¬ Bs order = [] := fun he => ho ((Bs_nil order).1 he)
      simp only [e, if_false, List.length_append] at h
      step*
      all_goals first | (emit [Sql.abs, Pg.render, hnil, e])
  | Insert t cols conflict update =>
    simp only [Sql.abs, Pg.render, List.length_append] at h
    have hc : cols.len.val ≤ U32.max := by simpa using hs
    by_cases hu : update.length = 0
    · have e : Bs update = [] := (Bs_nil update).2 hu
      simp only [e, if_true] at h
      step*
      all_goals first | (emit [Sql.abs, Pg.render, hnil, e, Bs_len])
    · have e : ¬ Bs update = [] := fun he => hu ((Bs_nil update).1 he)
      have es : (fun n => Pg.quote n ++ Pg.lit " = EXCLUDED." ++ Pg.quote n) = setText := rfl
      simp only [e, if_false, List.length_append, es] at h
      step*
      all_goals first | (emit [hnil, e, Bs_len]; done) | (emit [Sql.abs, Pg.render, hnil, e, Bs_len, setText_eq])
  | Delete t conds =>
    simp only [Sql.abs, Pg.render, List.length_append] at h
    step* <;> emit [Sql.abs, Pg.render, hnil]

end h5i_app_pgsql
