import Abs
/-!
# The extracted compiler computes `H5iAppLib.Pg`'s statements

`valid` decides `Pg.Valid`; `create`, `select`, `compile` return
`Pg.createA`, `Pg.selectA`, `Pg.compileA` of the abstracted arguments.
-/
open Aeneas Aeneas.Std Result H5iAppLib

namespace h5i_app_pgsql.Comp

theorem B_len (v : alloc.vec.Vec U8) : (B v).length = v.length := by simp [B]

theorem B_inj {v w : alloc.vec.Vec U8} (h : B v = B w) : v = w := by
  apply alloc.vec.Vec.eq_iff _ _ |>.2
  apply List.map_injective_iff.2 (fun a b hab => by simpa [u8_eq_iff] using hab) h

@[step]
theorem push_lit_loop_spec (out : alloc.vec.Vec U8) (s : Slice U8) (i : Usize) (hi : i.val ≤ s.length)
    (hb : out.length + s.length ≤ Usize.max) :
    push_lit_loop out s i ⦃ r => r.val = out.val ++ s.val.drop i.val ⦄ := by
  unfold push_lit_loop
  apply WP.spec_mono (loop_fold s.val (fun v : alloc.vec.Vec U8 => v.val) (fun acc x => acc ++ [x])
    (fun o k => o.length + (s.length - k) ≤ Usize.max) (fun x => push_lit_loop.body s x.1 x.2) ?_ out i
    hi (by simp only [alloc.vec.Vec.length, Slice.length] at *; omega))
  · intro r hr; rw [hr, foldl_snoc]
  · intro o j hj ho
    unfold push_lit_loop.body
    dsimp only
    split
    · rename_i hlt
      step*
      simp only [FoldStep]
      refine ⟨by scalar_tac, by rw [out1_post, i2_post], by scalar_tac, ?_⟩
      have hl : out1.length = o.length + 1 := by simp [alloc.vec.Vec.length, out1_post]
      have hi3 : i3.val = j.val + 1 := by scalar_tac
      have hjs : j.val < s.length := by scalar_tac
      rw [hl, hi3]; omega
    · simp only [WP.spec_ok, FoldStep, and_true]; scalar_tac

@[step]
theorem push_lit_spec (out : alloc.vec.Vec U8) (s : Slice U8) (hb : out.length + s.length ≤ Usize.max) :
    push_lit out s ⦃ r => r.val = out.val ++ s.val ⦄ := by
  unfold push_lit
  apply WP.spec_mono (push_lit_loop_spec out s 0#usize (by simp) hb)
  intro r hr; simpa using hr

@[step]
theorem tenant_col_spec : tenant_col ⦃ v => B v = Pg.tenantName ∧ v.length = 9 ⦄ := by
  unfold tenant_col
  step*
  subst s_post
  simp [B, v_post, Pg.tenantName, Pg.lit, Array.to_slice, Array.make]

/-- A loop that returns `false` at the first failure of `Q` computes `all Q`. -/
theorem all_loop {α : Type} (l : List α) (Q : α → Bool) (body : Usize → Result (ControlFlow Usize Bool))
    (hstep : ∀ i : Usize, i.val ≤ l.length →
      body i ⦃ SearchStep l (fun x => !Q x) id (fun _ _ => false) true i ⦄)
    (i : Usize) (hi : i.val ≤ l.length) : loop body i ⦃ b => b = (l.drop i.val).all Q ⦄ := by
  apply WP.spec_mono (loop_search l (fun x => !Q x) id (fun _ _ => false) true body hstep i hi)
  intro b hb
  simp only [id] at hb
  rw [hb, searchFrom_const]
  cases h : (l.drop i.val).all Q <;> simp_all [List.all_eq_true, List.any_eq_true]

@[step]
theorem name_ok_spec (n : alloc.vec.Vec U8) : name_ok n ⦃ b => (b = true ↔ Pg.NameOk (B n)) ⦄ := by
  unfold name_ok name_ok_loop
  have hloop := all_loop n.val (fun x => !decide (x = 0#u8)) (fun i => name_ok_loop.body n i) (by
    intro i hi
    unfold name_ok_loop.body
    dsimp only
    split
    · rename_i hlt
      step*
      · simp only [SearchStep]; left; exact ⟨by scalar_tac, by simp_all, rfl⟩
      · simp only [SearchStep]; exact ⟨by scalar_tac, by simp_all, by scalar_tac⟩
    · simp only [WP.spec_ok, SearchStep]; right; exact ⟨by scalar_tac, rfl⟩) 0#usize (by simp)
  dsimp only
  split
  · rename_i h0
    simp only [WP.spec_ok, Pg.NameOk]
    have h0' : n.length = 0 := by scalar_tac
    have : n.val = [] := List.eq_nil_of_length_eq_zero h0'
    simp [B, this]
  · split
    · rename_i h0 h1
      simp only [WP.spec_ok, Pg.NameOk, B_len]
      have : 63 < n.length := by scalar_tac
      simp only [alloc.vec.Vec.length] at this
      simp; intro; omega
    · rename_i h0 h1
      apply WP.spec_mono hloop
      intro b hb
      rw [hb]
      have hne : n.length ≠ 0 := by scalar_tac
      have hle : n.length ≤ 63 := by scalar_tac
      simp only [Pg.NameOk, List.all_eq_true, B, List.mem_map]
      constructor
      · intro h
        refine ⟨by simpa [List.map_eq_nil_iff, alloc.vec.Vec.length] using hne, by simpa [alloc.vec.Vec.length] using hle, ?_⟩
        rintro ⟨x, hx, h0⟩
        have := h x hx
        simp [u8_eq_iff, h0] at this
      · rintro ⟨-, -, h⟩ x hx
        simp only [Bool.not_eq_eq_eq_not, Bool.not_true, decide_eq_false_iff_not]
        intro hx0
        exact h ⟨x, hx, by simp [hx0]⟩

/-- Per step of a loop checking `Q` at each index below `n`. -/
def AllStep (n : Nat) (Q : Nat → Prop) (i : Usize) : ControlFlow Usize Bool → Prop
  | .done b => (i.val < n ∧ ¬ Q i.val ∧ b = false) ∨ (n ≤ i.val ∧ b = true)
  | .cont j => i.val < n ∧ Q i.val ∧ j.val = i.val + 1

/-- Such a loop returns whether `Q` holds at every index from `i` to `n`. -/
theorem all_idx_loop (n : Nat) (Q : Nat → Prop) (body : Usize → Result (ControlFlow Usize Bool))
    (hstep : ∀ i : Usize, i.val ≤ n → body i ⦃ AllStep n Q i ⦄) (i : Usize) (hi : i.val ≤ n) :
    loop body i ⦃ b => (b = true ↔ ∀ j, i.val ≤ j → j < n → Q j) ⦄ := by
  apply loop.spec_decr_nat (measure := fun (j : Usize) => n - j.val)
    (inv := fun j => j.val ≤ n ∧ (∀ k, i.val ≤ k → k < j.val → Q k) ∧ i.val ≤ j.val)
  · rintro j ⟨hj, hq, hij⟩
    apply WP.spec_mono (hstep j hj)
    intro r h
    cases r with
    | done b =>
      rcases h with ⟨hlt, hn, rfl⟩ | ⟨hle, rfl⟩
      · simp only [Bool.false_eq_true, false_iff, not_forall]
        exact ⟨j.val, hij, hlt, hn⟩
      · simp only [true_iff]
        intro k hk hkn
        exact hq k hk (by omega)
    | cont k =>
      obtain ⟨hlt, hQ, hk⟩ := h
      refine ⟨⟨by omega, fun m hm hmk => ?_, by omega⟩, by omega⟩
      by_cases e : m = j.val
      · subst e; exact hQ
      · exact hq m hm (by omega)
  · exact ⟨hi, fun k hk hk' => absurd hk' (by omega), le_refl _⟩

theorem all_drop_iff {α : Type} (l : List α) (Q : α → Bool) (k : Nat) :
    (l.drop k).all Q = true ↔ ∀ j (hj : j < l.length), k ≤ j → Q l[j] = true := by
  rw [List.all_eq_true]
  constructor
  · intro h j hj hkj
    apply h
    rw [List.mem_iff_getElem]
    exact ⟨j - k, by simp; omega, by simp; congr 1; omega⟩
  · intro h x hx
    obtain ⟨m, hm, rfl⟩ := List.mem_iff_getElem.1 hx
    simp only [List.getElem_drop]
    exact h _ _ (by omega)

@[step]
theorem distinct_from_spec (names : alloc.vec.Vec (alloc.vec.Vec U8)) (i : Usize) (hi : i.val < names.length) :
    distinct_from names i ⦃ b => (b = true ↔ ∀ j (hj : j < names.length), i.val < j →
      names.val[i.val]'(by simpa using hi) ≠ names.val[j]) ⦄ := by
  unfold distinct_from distinct_from_loop
  step*
  apply WP.spec_mono (all_idx_loop names.length (fun j => ∀ hj : j < names.length,
    names.val[i.val]'(by simpa using hi) ≠ names.val[j]) _ ?_ j (by scalar_tac))
  · intro b hb
    rw [hb]
    constructor
    · intro h k hk hik; exact h k (by scalar_tac) hk hk
    · intro h k hk hkn hk'; exact h k hk' (by scalar_tac)
  · intro k hk
    unfold distinct_from_loop.body
    dsimp only
    split
    · rename_i hlt
      step*
      · refine Or.inl ⟨by scalar_tac, fun h => ?_, rfl⟩
        rename_i hb
        simp_all
      · refine ⟨by scalar_tac, fun hk' => ?_, by scalar_tac⟩
        rename_i hb
        simp_all
    · simp only [WP.spec_ok]; exact Or.inr ⟨by scalar_tac, rfl⟩

theorem nodup_iff_idx {α : Type} (l : List α) :
    l.Nodup ↔ ∀ a b (ha : a < l.length) (hb : b < l.length), a < b → l[a] ≠ l[b] := by
  rw [List.Nodup, List.pairwise_iff_getElem]

@[step]
theorem all_distinct_spec (names : alloc.vec.Vec (alloc.vec.Vec U8)) :
    all_distinct names ⦃ b => (b = true ↔ (Bs names).Nodup) ⦄ := by
  unfold all_distinct all_distinct_loop
  apply WP.spec_mono (all_idx_loop names.length (fun a => ∀ ha : a < names.length, ∀ b (hb : b < names.length), a < b →
    names.val[a]'(by simpa using ha) ≠ names.val[b]) _ ?_ 0#usize (by simp))
  · intro b hb
    rw [hb, Bs, List.nodup_map_iff (fun x y h => B_inj h), nodup_iff_idx]
    constructor
    · intro h a c ha hc hac
      exact h a (by simp) (by simpa using ha) (by simpa using ha) c (by simpa using hc) hac
    · intro h a _ ha ha' c hc hac
      exact h a c (by simpa using ha') (by simpa using hc) hac
  · intro k hk
    unfold all_distinct_loop.body
    dsimp only
    split
    · rename_i hlt
      step*
      · refine ⟨by scalar_tac, fun ha c hc hac => ?_, by scalar_tac⟩
        rename_i hb
        exact (b_post.1 hb) c hc hac
      · refine Or.inl ⟨by scalar_tac, fun h => ?_, rfl⟩
        rename_i hb
        exact hb (b_post.2 (fun c hc hkc => h (by scalar_tac) c hc hkc))
    · simp only [WP.spec_ok]; exact Or.inr ⟨by scalar_tac, rfl⟩

@[step]
theorem all_ok_spec (names : alloc.vec.Vec (alloc.vec.Vec U8)) :
    all_ok names ⦃ b => (b = true ↔ ∀ n ∈ Bs names, Pg.NameOk n) ⦄ := by
  unfold all_ok all_ok_loop
  apply WP.spec_mono (all_idx_loop names.length (fun a => ∀ ha : a < names.length,
    Pg.NameOk (B (names.val[a]'(by simpa using ha)))) _ ?_ 0#usize (by simp))
  · intro b hb
    rw [hb]
    constructor
    · intro h n hn
      obtain ⟨m, hm, rfl⟩ := List.mem_iff_getElem.1 hn
      simp only [Bs, List.getElem_map]
      exact h m (by simp) (by simpa [Bs] using hm) (by simpa [Bs] using hm)
    · intro h a _ ha ha'
      exact h _ (List.mem_map.2 ⟨_, List.getElem_mem _, rfl⟩)
  · intro k hk
    unfold all_ok_loop.body
    dsimp only
    split
    · rename_i hlt
      step*
      · refine ⟨by scalar_tac, fun _ => ?_, by scalar_tac⟩
        rename_i hb
        simp_all
      · refine Or.inl ⟨by scalar_tac, fun h => ?_, rfl⟩
        rename_i hb
        exact hb (b_post.2 (by simp_all))
    · simp only [WP.spec_ok]; exact Or.inr ⟨by scalar_tac, rfl⟩

@[step]
theorem col_names_spec (t : Table) (h : t.columns.length < 1600) :
    col_names t ⦃ v => Bs v = Pg.names t.abs ∧ v.length = t.columns.length + 1 ⦄ := by
  unfold col_names col_names_loop
  step*
  apply WP.spec_mono (loop_fold t.columns.val (fun v : alloc.vec.Vec (alloc.vec.Vec U8) => Bs v)
    (fun acc c => acc ++ [B c.name]) (fun o k => o.length = k + 1 ∧ k ≤ t.columns.length)
    (fun x => col_names_loop.body t x.1 x.2) ?_ out 0#usize (by simp) ?_)
  · intro r hr
    rw [foldl_map (fun c : Column => B c.name)] at hr
    refine ⟨?_, ?_⟩
    · rw [hr]; simp [Bs, out_post, Pg.names, Table.abs, Column.abs]; assumption
    · have := congrArg List.length hr
      simp [Bs, out_post] at this
      simp [alloc.vec.Vec.length]; omega
  · intro o j hj ho
    unfold col_names_loop.body
    dsimp only
    split
    · rename_i hlt
      step*
      refine ⟨by scalar_tac, by simp [Bs, out1_post, v_post, c_post], by scalar_tac, ?_, by scalar_tac⟩
      simp [alloc.vec.Vec.length, out1_post] at ho ⊢
      scalar_tac
    · simp only [WP.spec_ok, FoldStep, and_true]; scalar_tac
  · simp [alloc.vec.Vec.length, out_post]

@[step]
theorem reserved_spec (n : alloc.vec.Vec U8) : reserved n ⦃ b => (b = true ↔ Pg.Reserved (B n)) ⦄ := by
  unfold reserved
  have hlit : Pg.lit "h5i_" = [104, 53, 105, 95] := by simp [Pg.lit]
  simp only [Pg.Reserved, hlit]
  step*
  all_goals
    rcases hn : n.val with _ | ⟨a, _ | ⟨b, _ | ⟨c, _ | ⟨d, rest⟩⟩⟩⟩ <;>
      simp_all [B, alloc.vec.Vec.length, u8_eq_iff] <;> scalar_tac

theorem abs_cols_length (t : Table) : t.abs.cols.length = t.columns.length := by
  simp [Table.abs, alloc.vec.Vec.length]

@[step]
theorem table_ok_spec (t : Table) : table_ok t ⦃ b => (b = true ↔ Pg.TabOk t.abs) ⦄ := by
  unfold table_ok
  have hl := abs_cols_length t
  have hname : t.abs.name = B t.name := rfl
  have hkl : t.abs.kl = t.key_len.val := rfl
  step with name_ok_spec as ⟨ b, hb ⟩
  split
  · rename_i h1
    step with reserved_spec as ⟨ b1, hb1 ⟩
    split
    · rename_i h2
      simp only [WP.spec_ok, Bool.false_eq_true, false_iff]
      exact fun hT => hT.2.1 (hname ▸ hb1.1 h2)
    · rename_i h2
      split
      · rename_i h3
        simp only [WP.spec_ok, Bool.false_eq_true, false_iff]
        intro hT
        apply hT.2.2.1
        have : t.columns.length = 0 := by scalar_tac
        rw [← List.length_eq_zero_iff, hl, this]
      · rename_i h3
        split
        · rename_i h4
          simp only [WP.spec_ok, Bool.false_eq_true, false_iff]
          intro hT
          have := hT.2.2.2.1
          rw [hl] at this
          scalar_tac
        · rename_i h4
          step with col_names_spec t (by scalar_tac) as ⟨ names, hn1, hn2 ⟩
          step*
          · rw [b_post, hn1]
            constructor
            · intro hnd
              refine ⟨hname ▸ hb.1 h1, fun hr => h2 (hb1.2 (hname ▸ hr)), fun he => ?_, ?_, ?_,
                hn1 ▸ b2_post.1 ‹b2 = true›, hnd⟩
              · have := congrArg List.length he
                rw [hl] at this; simp at this; scalar_tac
              · rw [hl]; scalar_tac
              · rw [hkl, hl]; scalar_tac
            · intro hT; exact hT.2.2.2.2.2.2
          · simp only [Bool.false_eq_true, false_iff]
            intro hT
            exact ‹¬b2 = true› (b2_post.2 (hn1 ▸ hT.2.2.2.2.2.1))
          · simp only [Bool.false_eq_true, false_iff]
            intro hT
            have := hT.2.2.2.2.1
            rw [hkl, hl] at this
            scalar_tac
  · rename_i h1
    simp only [WP.spec_ok, Bool.false_eq_true, false_iff]
    exact fun hT => h1 (hb.2 (hname ▸ hT.1))

@[step]
theorem valid_spec (ts : alloc.vec.Vec Table) : valid ts ⦃ b => (b = true ↔ Pg.Valid (tabs ts)) ⦄ := by
  unfold valid valid_loop
  apply loop.spec_decr_nat (measure := fun (x : alloc.vec.Vec (alloc.vec.Vec U8) × Usize) => ts.length - x.2.val)
    (inv := fun x => x.2.val ≤ ts.length ∧ Bs x.1 = (ts.val.take x.2.val).map (fun t => B t.name) ∧
      x.1.length = x.2.val ∧ ∀ k (hk : k < ts.val.length), k < x.2.val → Pg.TabOk ts.val[k].abs)
  · rintro ⟨names, j⟩ ⟨hj, hbs, hlen, hok⟩
    simp only at hj hbs hlen hok
    unfold valid_loop.body
    dsimp only
    split
    · rename_i hlt
      step*
      · have hb : b = true := ‹_›
        refine ⟨by scalar_tac, ?_, ?_, fun k hk hkj => ?_, by scalar_tac⟩
        · have hj' : j.val < ts.val.length := by scalar_tac
          simp only [Bs] at hbs ⊢
          rw [names1_post, List.map_append, hbs, i2_post, List.take_succ_eq_append_getElem hj',
            List.map_append, ← t_post, v_post]
          rfl
        · simp only [alloc.vec.Vec.length, names1_post, List.length_append, List.length_singleton, i2_post]
          simp only [alloc.vec.Vec.length] at hlen; omega
        · by_cases e : k = j.val
          · subst e; rw [← t_post]; exact b_post.1 hb
          · exact hok k hk (by scalar_tac)
      · have hb : ¬ b = true := ‹_›
        simp only [Bool.false_eq_true, false_iff]
        intro hv
        apply hb
        apply b_post.2
        rw [t_post]
        exact hv.1 _ (List.mem_map.2 ⟨_, List.getElem_mem _, rfl⟩)
    · step*
      rw [b_post]
      have hjl : j.val = ts.val.length := by scalar_tac
      have e : Bs names = (tabs ts).map (·.name) := by
        rw [hbs, hjl, List.take_length]; simp [tabs, Table.abs]
      rw [e]
      constructor
      · intro hnd
        refine ⟨fun t ht => ?_, hnd⟩
        obtain ⟨k, hk, rfl⟩ := List.mem_iff_getElem.1 ht
        simp only [tabs, List.getElem_map]
        exact hok k (by simpa [tabs] using hk) (by simp [tabs] at hk; omega)
      · exact fun hv => hv.2
  · exact ⟨by simp, by simp [Bs], by simp, fun k _ hk => absurd hk (by simp)⟩

theorem val_clone (x : h5i_app_sql.Val) : h5i_app_sql.Val.Insts.CoreCloneClone.clone x = ok x := by
  cases x <;> simp [h5i_app_sql.Val.Insts.CoreCloneClone.clone, u8vec_clone, lift]

@[step]
theorem val_clone_spec (x : h5i_app_sql.Val) : h5i_app_sql.Val.Insts.CoreCloneClone.clone x ⦃ y => y = x ⦄ := by
  simp [val_clone]

@[step]
theorem has_kind_spec (v : h5i_app_sql.Val) (k : Kind) :
    has_kind v k ⦃ b => (b = true ↔ Pg.Typed valKind k.abs v) ⦄ := by
  unfold has_kind kind_eq
  cases v <;> cases k <;> simp [Pg.Typed, valKind, Kind.abs]

theorem cols_abs_get (t : Table) (i : Nat) (hi : i < t.columns.length) :
    t.abs.cols[i]? = some (t.columns.val[i]'(by simpa using hi)).abs := by
  simp only [Table.abs, List.getElem?_map, List.getElem?_eq_getElem (show i < t.columns.val.length by simpa using hi)]
  rfl

@[step]
theorem fits_spec (t : Table) (i : Usize) (v : h5i_app_sql.Val) (hi : i.val < t.columns.length) :
    fits t i v ⦃ b => (b = true ↔ Pg.FitsAt valKind t.abs i.val v) ⦄ := by
  unfold fits is_null
  have hc := cols_abs_get t i.val hi
  simp only [Pg.FitsAt, hc, Option.some.injEq, exists_eq_left', Column.abs]
  cases v <;> step* <;> simp_all [valKind, Pg.Typed, Table.abs] <;> scalar_tac

@[step]
theorem all_fit_spec (t : Table) (off : Usize) (vs : alloc.vec.Vec h5i_app_sql.Val)
    (h : off.val + vs.length ≤ t.columns.length) :
    all_fit t off vs ⦃ b => (b = true ↔ ∀ i (hi : i < vs.length),
      Pg.FitsAt valKind t.abs (off.val + i) (vs.val[i]'(by simpa using hi))) ⦄ := by
  unfold all_fit all_fit_loop
  apply WP.spec_mono (all_idx_loop vs.length (fun i => ∀ hi : i < vs.length,
    Pg.FitsAt valKind t.abs (off.val + i) (vs.val[i]'(by simpa using hi))) _ ?_ 0#usize (by simp))
  · intro b hb
    rw [hb]
    constructor
    · intro h i hi; exact h i (by simp) hi hi
    · intro h i _ hi hi'; exact h i hi'
  · intro k hk
    unfold all_fit_loop.body
    dsimp only
    split
    · rename_i hlt
      step*
      · refine ⟨by scalar_tac, fun _ => ?_, by scalar_tac⟩
        have := b_post.1 ‹_›
        simp_all
      · refine Or.inl ⟨by scalar_tac, fun hf => ?_, rfl⟩
        apply ‹¬ b = true›
        apply b_post.2
        have := hf (by scalar_tac)
        simp_all
    · simp only [WP.spec_ok]; exact Or.inr ⟨by scalar_tac, rfl⟩

@[step]
theorem names_between_spec (t : Table) (lo hi : Usize) (hlh : lo.val ≤ hi.val) (hhc : hi.val ≤ t.columns.length)
    (h : t.columns.length < 1600) :
    names_between t lo hi ⦃ v => Bs v = ((t.columns.val.take hi.val).drop lo.val).map (fun c => B c.name) ⦄ := by
  unfold names_between names_between_loop
  apply loop.spec_decr_nat (measure := fun (x : alloc.vec.Vec (alloc.vec.Vec U8) × Usize) => hi.val - x.2.val)
    (inv := fun x => lo.val ≤ x.2.val ∧ x.2.val ≤ hi.val ∧
      Bs x.1 = ((t.columns.val.take x.2.val).drop lo.val).map (fun c => B c.name))
  · rintro ⟨o, j⟩ ⟨hlo, hj, hbs⟩
    simp only at hlo hj hbs
    have hlen : o.length = j.val - lo.val := by
      have := congrArg List.length hbs
      simp only [Bs, List.length_map, List.length_drop, List.length_take] at this
      simp only [alloc.vec.Vec.length]; rw [this]; scalar_tac
    unfold names_between_loop.body
    dsimp only
    split
    · split
      · step*
        refine ⟨by scalar_tac, by scalar_tac, ?_, by scalar_tac⟩
        have hjc : j.val < t.columns.val.length := by scalar_tac
        simp only [Bs] at hbs ⊢
        rw [out1_post, List.map_append, hbs, i2_post, List.take_succ_eq_append_getElem hjc,
          List.drop_append_of_le_length (by simp; omega)]
        simp [v_post, c_post]
      · exfalso; scalar_tac
    · simp only [WP.spec_ok]
      have : j.val = hi.val := by scalar_tac
      rw [hbs, this]
  · exact ⟨le_refl _, hlh, by simp [Bs]⟩

/-- The column definition `create` gives column `i`. -/
def defAt (n : Nat) (i : Nat) (c : Pg.Col) : Pg.ColDef := ⟨c.name, c.kind, !c.nullable || decide (i < n)⟩

theorem mapIdx_take_succ {α β : Type} (f : Nat → α → β) (l : List α) (j : Nat) (hj : j < l.length) :
    (l.take (j + 1)).mapIdx f = (l.take j).mapIdx f ++ [f j l[j]] := by
  rw [List.take_succ_eq_append_getElem hj, List.mapIdx_append]
  simp [Nat.min_eq_left (Nat.le_of_lt hj)]

theorem create_loop_spec (t : Table) (cols : alloc.vec.Vec ColDef) (key : alloc.vec.Vec (alloc.vec.Vec U8))
    (n : Usize) (hn : n.val = t.key_len.val) (h : t.columns.length < 1600)
    (hc0 : cols.val.map ColDef.abs = [⟨Pg.tenantName, .int, true⟩]) (hk0 : Bs key = [Pg.tenantName]) :
    create_loop t.columns cols key n 0#usize ⦃ r =>
      r.1.val.map ColDef.abs = Pg.colDefs t.abs ∧ Bs r.2 = Pg.keyNames t.abs ⦄ := by
  unfold create_loop
  apply loop.spec_decr_nat
    (measure := fun (x : alloc.vec.Vec ColDef × alloc.vec.Vec (alloc.vec.Vec U8) × Usize) => t.columns.length - x.2.2.val)
    (inv := fun x => x.2.2.val ≤ t.columns.length ∧ x.1.length = x.2.2.val + 1 ∧ x.2.1.length ≤ x.2.2.val + 1 ∧
      x.1.val.map ColDef.abs = ⟨Pg.tenantName, .int, true⟩ ::
        ((t.columns.val.take x.2.2.val).map Column.abs).mapIdx (defAt t.key_len.val) ∧
      Bs x.2.1 = Pg.tenantName :: (t.columns.val.take (min x.2.2.val t.key_len.val)).map (fun c => B c.name))
  · rintro ⟨cs, ks, j⟩ ⟨hj, hcl, hkl, hcs, hks⟩
    simp only at hj hcl hkl hcs hks
    unfold create_loop.body
    dsimp only
    split
    · rename_i hlt
      have hjc : j.val < t.columns.val.length := by scalar_tac
      have hcs' : ∀ (x : alloc.vec.Vec ColDef) (nn : Bool), x.val = cs.val ++ [⟨t.columns.val[j.val].name,
          t.columns.val[j.val].kind, nn⟩] → nn = (!t.columns.val[j.val].nullable || decide (j.val < t.key_len.val)) →
          x.val.map ColDef.abs = ⟨Pg.tenantName, .int, true⟩ ::
            ((t.columns.val.take (j.val + 1)).map Column.abs).mapIdx (defAt t.key_len.val) := by
        intro x nn hx hnn
        rw [hx, List.map_append, hcs, List.map_take, List.map_take,
          mapIdx_take_succ _ _ _ (by simpa using hjc)]
        simp [ColDef.abs, defAt, Column.abs, hnn]
      have hlen' : ∀ (x : alloc.vec.Vec ColDef) (c : ColDef), x.val = cs.val ++ [c] → x.length = j.val + 1 + 1 := by
        intro x c hx; simp only [alloc.vec.Vec.length] at *; simp [hx, hcl]
      have hkp : ∀ (x : alloc.vec.Vec (alloc.vec.Vec U8)), x.val = ks.val ++ [t.columns.val[j.val].name] →
          j.val < t.key_len.val →
          Bs x = Pg.tenantName :: (t.columns.val.take (min (j.val + 1) t.key_len.val)).map (fun c => B c.name) := by
        intro x hx hjk
        simp only [Bs] at hks ⊢
        rw [hx, List.map_append, hks, Nat.min_eq_left (by omega), Nat.min_eq_left (by omega)]
        simp only [List.map_take, List.cons_append, List.cons.injEq, true_and]
        rw [List.take_succ_eq_append_getElem (by simpa using hjc)]
        simp
      have hkn : ¬ j.val < t.key_len.val →
          Bs ks = Pg.tenantName :: (t.columns.val.take (min (j.val + 1) t.key_len.val)).map (fun c => B c.name) := by
        intro hjk
        rw [hks, Nat.min_eq_right (by omega), Nat.min_eq_right (by omega)]
      step*
      all_goals (split <;> step*)
      all_goals (refine ⟨by scalar_tac, by rw [i2_post]; exact hlen' _ _ cols1_post, ?_,
        by rw [i2_post]; exact hcs' _ _ (by rw [cols1_post, v1_post, c_post]) (by simp_all), ?_, by scalar_tac⟩)
      all_goals try (rw [i2_post]; refine hkp _ ?_ (by scalar_tac); rw [key1_post, x_post, c_post]; done)
      all_goals try (rw [i2_post]; exact hkn (by scalar_tac))
      all_goals try (have := congrArg List.length key1_post; simp only [alloc.vec.Vec.length] at *; simp at this; omega)
      all_goals done
    · simp only [WP.spec_ok]
      have hjl : j.val = t.columns.val.length := by scalar_tac
      refine ⟨?_, ?_⟩
      · rw [hcs, hjl, List.take_length]
        simp [Pg.colDefs, Table.abs]
        rfl
      · rw [hks, hjl]
        simp only [Pg.keyNames, Table.abs, List.map_take, List.map_map, List.cons.injEq, true_and]
        rw [← List.take_take, List.take_of_length_le (l := List.take _ _) (by simp)]
        rfl
  · refine ⟨by simp, ?_, ?_, ?_, ?_⟩
    · have := congrArg List.length hc0; simpa using this
    · have := congrArg List.length hk0; simp [Bs] at this; simp [alloc.vec.Vec.length, this]
    · simpa using hc0
    · simpa using hk0

@[step]
theorem create_spec (t : Table) (h : t.columns.length < 1600) : create t ⦃ s => s.abs = Pg.createA t.abs ⦄ := by
  unfold create
  step*
  have hc0 : cols.val.map ColDef.abs = [⟨Pg.tenantName, .int, true⟩] := by
    simp [cols_post, ColDef.abs, Kind.abs]; assumption
  have hk0 : Bs key = [Pg.tenantName] := by simp [Bs, key_post]; assumption
  step with create_loop_spec t cols key n (by scalar_tac) h hc0 hk0 as ⟨ r, k, hr1, hr2 ⟩
  step*
  simp only [Sql.abs, Pg.createA, hr1, hr2, v1_post]
  rfl

theorem tabs_get (ts : alloc.vec.Vec Table) (i : Nat) (hi : i < ts.length) :
    (tabs ts)[i]? = some (ts.val[i]'(by simpa using hi)).abs := by
  simp only [tabs, List.getElem?_map, List.getElem?_eq_getElem (show i < ts.val.length by simpa using hi)]
  rfl

theorem tabs_none (ts : alloc.vec.Vec Table) (i : Nat) (hi : ts.length ≤ i) : (tabs ts)[i]? = none := by
  simp [tabs]; simpa using hi

theorem cols_small {ts : alloc.vec.Vec Table} (hv : Pg.Valid (tabs ts)) (i : Nat) (hi : i < ts.length) :
    (ts.val[i]'(by simpa using hi)).columns.length < 1600 ∧
      (ts.val[i]'(by simpa using hi)).key_len.val ≤ (ts.val[i]'(by simpa using hi)).columns.length := by
  have := hv.1 _ (List.mem_of_getElem? (tabs_get ts i hi))
  have h1 := this.2.2.2.1
  have h2 := this.2.2.2.2.1
  rw [abs_cols_length] at h1 h2
  exact ⟨h1, h2⟩

theorem names_all (t : Table) : (t.columns.val.take t.columns.length).drop 0 = t.columns.val := by
  simp [alloc.vec.Vec.length]

@[step]
theorem select_spec (ts : alloc.vec.Vec Table) (tenant : I64) (table : U32) (filter : Option (U32 × h5i_app_sql.Val)) :
    select ts tenant table filter ⦃ o => o.map Query.abs =
      Pg.selectA valKind (tabs ts) (h5i_app_sql.Val.Int tenant) table.val (filter.map (fun p => (p.1.val, p.2))) ⦄ := by
  unfold select
  step with valid_spec as ⟨ b, hb ⟩
  split
  · rename_i hbt
    have hv := hb.1 hbt
    simp only [Pg.selectA, if_pos hv]
    step as ⟨ i, hi ⟩
    split
    · simp only [WP.spec_ok, Option.map_none]
      rw [tabs_none ts _ (by scalar_tac)]
    · rename_i hlt
      step as ⟨ i2, hi2 ⟩
      step as ⟨ t, ht ⟩
      have hi2' : i2.val < ts.length := by scalar_tac
      have htb : (tabs ts)[table.val]? = some t.abs := by
        rw [ht, show table.val = i2.val by scalar_tac]; exact tabs_get ts _ hi2'
      obtain ⟨hs1, hs2⟩ := cols_small hv i2.val hi2'
      rw [← ht] at hs1 hs2
      rw [htb]
      have hna : Bs (alloc.vec.Vec.new (alloc.vec.Vec U8)) = [] := rfl
      have hcols : (t.abs.cols.map (·.name)) = t.columns.val.map (fun c => B c.name) := by
        simp [Table.abs, Column.abs]
      have hkey : ((t.abs.cols.take t.abs.kl).map (·.name)) = (t.columns.val.take t.key_len.val).map (fun c => B c.name) := by
        simp only [Table.abs, Column.abs, List.map_take, List.map_map]; rfl
      rcases filter with _ | ⟨col, val⟩
      · step*
        simp only [Option.map_some, Query.abs, Sql.abs, Option.map_none, Option.some.injEq, Prod.mk.injEq,
          hcols, hkey]
        refine ⟨?_, by simp [params_post]⟩
        simp only [Pg.Sql.select.injEq]
        refine ⟨by rw [v1_post]; rfl, ?_, ?_, ?_⟩
        · rw [v2_post, List.drop_zero, List.take_of_length_le (by simp [alloc.vec.Vec.len, alloc.vec.Vec.length])]
        · simp [conds_post, Cond.abs]; assumption
        · rw [v3_post, List.drop_zero, i4_post]; simp
      · step*
        · simp only [Option.map_some, Option.map_none]
          rw [List.getElem?_eq_none (by rw [abs_cols_length]; scalar_tac)]
        · simp only [Option.map_some]
          have hc3 : col.val < t.columns.length := by scalar_tac
          have e3 : i3.val = col.val := by scalar_tac
          rw [cols_abs_get t _ hc3]
          simp only
          have hty : Pg.Typed valKind (t.columns.val[col.val]'(by simpa using hc3)).abs.kind val := by
            have := b1_post.1 ‹_›; rw [c_post] at this; simpa [e3, Column.abs] using this
          rw [if_pos hty]
          simp only [Option.map_some, Query.abs, Sql.abs, Option.some.injEq, Prod.mk.injEq, hcols, hkey,
            Pg.Sql.select.injEq]
          refine ⟨⟨by rw [v2_post]; rfl, ?_, ?_, ?_⟩, by simp [params1_post, params_post]⟩
          · rw [v3_post, List.drop_zero, List.take_of_length_le (by simp [alloc.vec.Vec.len, alloc.vec.Vec.length])]
          · simp [conds1_post, conds_post, Cond.abs, v1_post, c_post, e3, Column.abs]; assumption
          · rw [v4_post, List.drop_zero, i6_post]; simp
        · simp only [Option.map_some, Option.map_none]
          have hc3 : col.val < t.columns.length := by scalar_tac
          have e3 : i3.val = col.val := by scalar_tac
          rw [cols_abs_get t _ hc3]
          simp only
          have hty : ¬ Pg.Typed valKind (t.columns.val[col.val]'(by simpa using hc3)).abs.kind val := by
            intro h; apply ‹¬ b1 = true›; apply b1_post.2; rw [c_post]; simpa [e3, Column.abs] using h
          rw [if_neg hty]
  · rename_i hbf
    simp only [WP.spec_ok, Option.map_none, Pg.selectA]
    rw [if_neg (fun hv => hbf (hb.2 hv))]

/-- Pushing a clone of each value, in order. -/
theorem push_vals_loop {body : alloc.vec.Vec h5i_app_sql.Val × Usize →
      Result (ControlFlow (alloc.vec.Vec h5i_app_sql.Val × Usize) (alloc.vec.Vec h5i_app_sql.Val))}
    (vs params : alloc.vec.Vec h5i_app_sql.Val)
    (hbody : ∀ (o : alloc.vec.Vec h5i_app_sql.Val) (j : Usize), body (o, j) = (if j < vs.len then do
        let v ← alloc.vec.Vec.index (core.slice.index.SliceIndexUsizeSlice h5i_app_sql.Val) vs j
        let v1 ← h5i_app_sql.Val.Insts.CoreCloneClone.clone v
        let o1 ← alloc.vec.Vec.push o v1
        let j1 ← j + 1#usize
        ok (ControlFlow.cont (o1, j1))
      else ok (ControlFlow.done o)))
    (hb : params.length + vs.length ≤ Usize.max) :
    loop body (params, 0#usize) ⦃ r => r.val = params.val ++ vs.val ⦄ := by
  apply WP.spec_mono (loop_fold vs.val (fun v : alloc.vec.Vec h5i_app_sql.Val => v.val) (fun acc x => acc ++ [x])
    (fun o k => o.length + (vs.length - k) ≤ Usize.max) body ?_ params 0#usize (by simp) (by simpa using hb))
  · intro r hr; rw [hr, foldl_snoc]; simp
  · intro o j hj ho
    rw [hbody]
    split
    · step*
      simp only [FoldStep]
      refine ⟨by scalar_tac, by rw [o1_post, v1_post, v_post], by scalar_tac, ?_⟩
      have hl : o1.length = o.length + 1 := by simp [alloc.vec.Vec.length, o1_post]
      have : j.val < vs.length := by scalar_tac
      rw [hl, j1_post]; omega
    · simp only [WP.spec_ok, FoldStep, and_true]; scalar_tac

@[step]
theorem compile_loop0_spec (key params : alloc.vec.Vec h5i_app_sql.Val) (hb : params.length + key.length ≤ Usize.max) :
    compile_loop0 key params 0#usize ⦃ r => r.val = params.val ++ key.val ⦄ := by
  unfold compile_loop0
  exact push_vals_loop key params (fun o j => by simp only [compile_loop0.body]) hb

@[step]
theorem compile_loop1_spec (rest params : alloc.vec.Vec h5i_app_sql.Val) (hb : params.length + rest.length ≤ Usize.max) :
    compile_loop1 rest params 0#usize ⦃ r => r.val = params.val ++ rest.val ⦄ := by
  unfold compile_loop1
  exact push_vals_loop rest params (fun o j => by simp only [compile_loop1.body]) hb

@[step]
theorem compile_loop2_spec (cols : alloc.vec.Vec Column) (n : Usize) (conflict : alloc.vec.Vec (alloc.vec.Vec U8))
    (hn : n.val ≤ cols.length) (hb : conflict.length + n.val ≤ Usize.max) :
    compile_loop2 cols n conflict 0#usize ⦃ r => Bs r = Bs conflict ++ (cols.val.take n.val).map (fun c => B c.name) ⦄ := by
  unfold compile_loop2
  apply loop.spec_decr_nat (measure := fun (x : alloc.vec.Vec (alloc.vec.Vec U8) × Usize) => n.val - x.2.val)
    (inv := fun x => x.2.val ≤ n.val ∧ x.1.length = conflict.length + x.2.val ∧
      Bs x.1 = Bs conflict ++ (cols.val.take x.2.val).map (fun c => B c.name))
  · rintro ⟨o, k⟩ ⟨hk, hl, hbs⟩
    simp only at hk hl hbs
    unfold compile_loop2.body
    dsimp only
    split
    · step*
      have hkc : k.val < cols.val.length := by scalar_tac
      refine ⟨by scalar_tac, ?_, ?_, by scalar_tac⟩
      · simp only [alloc.vec.Vec.length] at *; simp [conflict1_post, hl, k1_post]; omega
      · simp only [Bs] at hbs ⊢
        rw [conflict1_post, List.map_append, hbs, k1_post, List.take_succ_eq_append_getElem hkc]
        simp only [List.map_append, List.append_assoc, List.map_cons, List.map_nil, v1_post, c_post]
    · simp only [WP.spec_ok]
      rw [hbs, show k.val = n.val by scalar_tac]
  · exact ⟨by simp, by simp, by simp⟩

@[step]
theorem compile_loop3_spec (key : alloc.vec.Vec h5i_app_sql.Val) (cols : alloc.vec.Vec Column)
    (conds : alloc.vec.Vec Cond) (params : alloc.vec.Vec h5i_app_sql.Val)
    (hk : key.length ≤ cols.length) (hc : cols.length < 1600) (hl1 : conds.length ≤ 1) (hl2 : params.length ≤ 1) :
    compile_loop3 key cols conds params 0#usize ⦃ r =>
      r.1.val.map Cond.abs = conds.val.map Cond.abs ++
        ((cols.val.take key.length).map Column.abs).mapIdx (fun i c => Pg.Cond.eq c.name (i + 2)) ∧
      r.2.val = params.val ++ key.val ⦄ := by
  unfold compile_loop3
  apply loop.spec_decr_nat (measure := fun (x : alloc.vec.Vec Cond × alloc.vec.Vec h5i_app_sql.Val × Usize) =>
      key.length - x.2.2.val)
    (inv := fun x => x.2.2.val ≤ key.length ∧ x.1.length = conds.length + x.2.2.val ∧
      x.2.1.length = params.length + x.2.2.val ∧
      x.1.val.map Cond.abs = conds.val.map Cond.abs ++
        ((cols.val.take x.2.2.val).map Column.abs).mapIdx (fun i c => Pg.Cond.eq c.name (i + 2)) ∧
      x.2.1.val = params.val ++ key.val.take x.2.2.val)
  · rintro ⟨cs, ps, j⟩ ⟨hj, hcl, hpl, hcs, hps⟩
    simp only at hj hcl hpl hcs hps
    unfold compile_loop3.body
    dsimp only
    split
    · have hjc : j.val < cols.val.length := by scalar_tac
      have hjk : j.val < key.val.length := by scalar_tac
      step*
      refine ⟨by scalar_tac, ?_, ?_, ?_, ?_, by scalar_tac⟩
      · simp only [alloc.vec.Vec.length] at *; simp [conds1_post, hcl, i4_post]; omega
      · simp only [alloc.vec.Vec.length] at *; simp [params1_post, hpl, i4_post]; omega
      · rw [conds1_post, List.map_append, hcs, i4_post, List.map_take, List.map_take,
          mapIdx_take_succ _ _ _ (by simpa using hjc)]
        have e3 : i3.val = j.val + 2 := by scalar_tac
        simp [Cond.abs, v1_post, c_post, Column.abs, e3]
      · rw [params1_post, hps, i4_post, List.take_succ_eq_append_getElem hjk, v3_post, v2_post]; simp
    · simp only [WP.spec_ok]
      have hjl : j.val = key.length := by scalar_tac
      rw [hjl] at hcs hps
      refine ⟨hcs, ?_⟩
      rw [hps, List.take_of_length_le (by simp)]
  · exact ⟨by simp, by simp, by simp, by simp, by simp⟩

theorem upsert_spec (ts : alloc.vec.Vec Table) (tenant : I64) (hv : Pg.Valid (tabs ts)) (table : U32)
    (key rest : alloc.vec.Vec h5i_app_sql.Val) :
    (do
      let i ← lift (UScalar.cast .Usize table)
      let i1 := alloc.vec.Vec.len ts
      if i >= i1
      then ok none
      else
        let i2 ← lift (UScalar.cast .Usize table)
        let t ← alloc.vec.Vec.index (core.slice.index.SliceIndexUsizeSlice Table) ts i2
        let n ← lift (UScalar.cast .Usize t.key_len)
        let i3 := alloc.vec.Vec.len key
        if i3 != n
        then ok none
        else
          let i4 := alloc.vec.Vec.len rest
          let i5 := alloc.vec.Vec.len t.columns
          let i6 ← i5 - n
          if i4 != i6
          then ok none
          else
            let b1 ← all_fit t 0#usize key
            if b1
            then
              let b2 ← all_fit t n rest
              if b2
              then
                let params ← alloc.vec.Vec.push (alloc.vec.Vec.new h5i_app_sql.Val) (h5i_app_sql.Val.Int tenant)
                let params1 ← compile_loop0 key params 0#usize
                let params2 ← compile_loop1 rest params1 0#usize
                let v ← tenant_col
                let conflict ← alloc.vec.Vec.push (alloc.vec.Vec.new (alloc.vec.Vec U8)) v
                let conflict1 ← compile_loop2 t.columns n conflict 0#usize
                let v1 ← alloc.vec.CloneVec.clone core.clone.CloneU8 t.name
                let v2 ← col_names t
                let i7 := alloc.vec.Vec.len t.columns
                let v3 ← names_between t n i7
                ok (some { sql := (Sql.Insert v1 v2 conflict1 v3), params := params2 })
              else ok none
            else ok none : Result (Option Query)) ⦃ o =>
      o.map Query.abs = Pg.compileA valKind (tabs ts) (h5i_app_sql.Val.Int tenant) (.up table.val key.val rest.val) ⦄ := by
  simp only [Pg.compileA, if_pos hv]
  step as ⟨ i, hi ⟩
  split
  · simp only [WP.spec_ok, Option.map_none]; rw [tabs_none ts _ (by scalar_tac)]
  · step as ⟨ i2, hi2 ⟩
    step as ⟨ t, ht ⟩
    have hi2' : i2.val < ts.length := by scalar_tac
    have htb : (tabs ts)[table.val]? = some t.abs := by
      rw [ht, show table.val = i2.val by scalar_tac]; exact tabs_get ts _ hi2'
    obtain ⟨hs1, hs2⟩ := cols_small hv i2.val hi2'
    rw [← ht] at hs1 hs2
    rw [htb]
    simp only
    have hkl : t.abs.kl = t.key_len.val := rfl
    have hcl := abs_cols_length t
    step as ⟨ n, hn ⟩
    have hn' : n.val = t.key_len.val := by scalar_tac
    split
    · rename_i hne
      simp only [WP.spec_ok, Option.map_none]
      rw [if_neg]
      intro hc; rw [hkl] at hc; have := hc.1; simp only [bne_iff_ne, ne_eq] at hne; apply hne; scalar_tac
    · rename_i hne
      have hkn : key.length = t.key_len.val := by simp only [bne_iff_ne, ne_eq, Decidable.not_not] at hne; scalar_tac
      step as ⟨ i6, hi6 ⟩
      split
      · rename_i hne2
        simp only [WP.spec_ok, Option.map_none]
        rw [if_neg]
        intro hc; rw [hkl, hcl] at hc; have := hc.2.1; simp only [bne_iff_ne, ne_eq] at hne2; apply hne2; scalar_tac
      · rename_i hne2
        have hrn : t.key_len.val + rest.length = t.columns.length := by
          simp only [bne_iff_ne, ne_eq, Decidable.not_not] at hne2; scalar_tac
        step with all_fit_spec t 0#usize key (by scalar_tac) as ⟨ b1, hb1 ⟩
        split
        · rename_i hb1t
          step with all_fit_spec t n rest (by scalar_tac) as ⟨ b2, hb2 ⟩
          split
          · rename_i hb2t
            have hcond : key.val.length = t.abs.kl ∧ t.abs.kl + rest.val.length = t.abs.cols.length ∧
                (∀ i (h : i < key.val.length), Pg.FitsAt valKind t.abs i key.val[i]) ∧
                ∀ i (h : i < rest.val.length), Pg.FitsAt valKind t.abs (t.abs.kl + i) rest.val[i] := by
              refine ⟨by rw [hkl]; simpa using hkn, by rw [hkl, hcl]; simpa using hrn, fun i h => ?_, fun i h => ?_⟩
              · have := (hb1.1 hb1t) i (by simpa using h); simpa using this
              · have := (hb2.1 hb2t) i (by simpa using h); rw [hn'] at this; exact this
            rw [if_pos hcond]
            step*
            simp only [Option.map_some, Query.abs, Sql.abs, Option.some.injEq, Prod.mk.injEq, Pg.Sql.insert.injEq]
            refine ⟨⟨by rw [v1_post]; rfl, v2_post, ?_, ?_⟩, by simp [params2_post, params1_post, params_post]⟩
            · rw [conflict1_post, hn']
              simp [Bs, conflict_post, Pg.keyNames, Table.abs, List.map_take]
              exact ⟨‹_›, rfl⟩
            · rw [v3_post, List.take_of_length_le (by simp [alloc.vec.Vec.len, alloc.vec.Vec.length]), hn']
              simp [Table.abs, List.map_drop]
              rfl
          · rename_i hb2f
            simp only [WP.spec_ok, Option.map_none]
            rw [if_neg]
            intro hc; apply hb2f; apply hb2.2; intro i h; rw [hn']; exact hc.2.2.2 i (by simpa using h)
        · rename_i hb1f
          simp only [WP.spec_ok, Option.map_none]
          rw [if_neg]
          intro hc; apply hb1f; apply hb1.2; intro i h; simpa using hc.2.2.1 i (by simpa using h)

/-- `compile` returns `Pg.compileA` of the abstracted schema, tenant and statement. -/
@[step]
theorem compile_spec (ts : alloc.vec.Vec Table) (tenant : I64) (s : h5i_app_sql.Stmt) :
    compile ts tenant s ⦃ o => o.map Query.abs = Pg.compileA valKind (tabs ts) (h5i_app_sql.Val.Int tenant) (Stmt.abs s) ⦄ := by
  unfold compile
  step with valid_spec as ⟨ b, hb ⟩
  split
  · rename_i hbt
    have hv := hb.1 hbt
    cases s with
    | Upsert table key rest => exact upsert_spec ts tenant hv table key rest
    | Delete table key =>
      simp only [Stmt.abs, Pg.compileA, if_pos hv]
      step as ⟨ i, hi ⟩
      split
      · simp only [WP.spec_ok, Option.map_none]; rw [tabs_none ts _ (by scalar_tac)]
      · step as ⟨ i2, hi2 ⟩
        step as ⟨ t, ht ⟩
        have hi2' : i2.val < ts.length := by scalar_tac
        have htb : (tabs ts)[table.val]? = some t.abs := by
          rw [ht, show table.val = i2.val by scalar_tac]; exact tabs_get ts _ hi2'
        obtain ⟨hs1, hs2⟩ := cols_small hv i2.val hi2'
        rw [← ht] at hs1 hs2
        rw [htb]
        simp only
        have hkl : t.abs.kl = t.key_len.val := rfl
        split
        · rename_i h0
          simp only [WP.spec_ok, Option.map_none]
          rw [if_neg]; intro hc; rw [hkl] at hc; have := hc.1; scalar_tac
        · rename_i h0
          step as ⟨ i4, hi4 ⟩
          split
          · rename_i hne
            simp only [WP.spec_ok, Option.map_none]
            rw [if_neg]
            intro hc; rw [hkl] at hc; have := hc.2.1; simp only [bne_iff_ne, ne_eq] at hne; apply hne; scalar_tac
          · rename_i hne
            have hkn : key.length = t.key_len.val := by simp only [bne_iff_ne, ne_eq, Decidable.not_not] at hne; scalar_tac
            step with all_fit_spec t 0#usize key (by scalar_tac) as ⟨ b1, hb1 ⟩
            split
            · rename_i hb1t
              have hcond : 0 < t.abs.kl ∧ key.val.length = t.abs.kl ∧
                  ∀ i (h : i < key.val.length), Pg.FitsAt valKind t.abs i key.val[i] := by
                refine ⟨by rw [hkl]; scalar_tac, by rw [hkl]; simpa using hkn, fun i h => ?_⟩
                have := (hb1.1 hb1t) i (by simpa using h); simpa using this
              rw [if_pos hcond]
              step*
              simp only [Option.map_some, Query.abs, Sql.abs, Option.some.injEq, Prod.mk.injEq,
                Pg.Sql.delete.injEq]
              refine ⟨⟨by rw [v1_post]; rfl, ?_⟩, by simp [conds1_post1, params_post]⟩
              rw [conds1_post, conds_post]
              simp only [List.map_append, List.map_cons, List.map_nil, Cond.abs, hkn,
                List.nil_append]
              rw [‹B v = Pg.tenantName›]
              simp [Table.abs, List.map_take]
            · rename_i hb1f
              simp only [WP.spec_ok, Option.map_none]
              rw [if_neg]
              intro hc; apply hb1f; apply hb1.2; intro i h; simpa using hc.2.2 i (by simpa using h)
    | DeleteWhere table col val =>
      simp only [Stmt.abs, Pg.compileA, if_pos hv]
      step as ⟨ i, hi ⟩
      split
      · simp only [WP.spec_ok, Option.map_none]; rw [tabs_none ts _ (by scalar_tac)]
      · step as ⟨ i2, hi2 ⟩
        step as ⟨ t, ht ⟩
        have hi2' : i2.val < ts.length := by scalar_tac
        have htb : (tabs ts)[table.val]? = some t.abs := by
          rw [ht, show table.val = i2.val by scalar_tac]; exact tabs_get ts _ hi2'
        rw [htb]
        simp only
        have hkl : t.abs.kl = t.key_len.val := rfl
        step as ⟨ i3, hi3 ⟩
        have e3 : i3.val = col.val := by scalar_tac
        split
        · rename_i h0
          simp only [WP.spec_ok, Option.map_none]
          split
          · rfl
          · rw [if_neg]; intro hc; rw [hkl] at hc; have := hc.1; scalar_tac
        · rename_i h0
          split
          · rename_i hge
            simp only [WP.spec_ok, Option.map_none]
            rw [List.getElem?_eq_none (by rw [abs_cols_length]; scalar_tac)]
          · rename_i hlt
            have hc3 : col.val < t.columns.length := by scalar_tac
            rw [cols_abs_get t _ hc3]
            simp only
            step as ⟨ c, hc ⟩
            step with has_kind_spec as ⟨ b1, hb1 ⟩
            split
            · rename_i hb1t
              have hcond : 0 < t.abs.kl ∧ Pg.Typed valKind (t.columns.val[col.val]'(by simpa using hc3)).abs.kind val := by
                refine ⟨by rw [hkl]; scalar_tac, ?_⟩
                have := hb1.1 hb1t; rw [hc] at this; simpa [e3, Column.abs] using this
              rw [if_pos hcond]
              step*
              simp only [Option.map_some, Query.abs, Sql.abs, Option.some.injEq, Prod.mk.injEq,
                Pg.Sql.delete.injEq]
              refine ⟨⟨by rw [v3_post]; rfl, ?_⟩, by simp [params1_post, params_post, v2_post]⟩
              rw [conds1_post, conds_post]
              simp only [List.map_append, List.map_cons, List.map_nil, Cond.abs, List.nil_append]
              rw [‹B v = Pg.tenantName›, v1_post, hc]
              simp [e3, Column.abs]
            · rename_i hb1f
              simp only [WP.spec_ok, Option.map_none]
              rw [if_neg]
              intro hcnd; apply hb1f; apply hb1.2; rw [hc]; simpa [e3, Column.abs] using hcnd.2
  · rename_i hbf
    simp only [WP.spec_ok, Option.map_none]
    cases s <;> simp only [Stmt.abs, Pg.compileA] <;> rw [if_neg (fun hv => hbf (hb.2 hv))]

end h5i_app_pgsql.Comp
