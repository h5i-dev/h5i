import Compile
import Render
/-!
# End to end, on the extracted compiler

Compiled statements are `Small`, print as `Pg.render`, and run as
`H5iAppLib.Sql.exec` says (`write_sound`); see also `select_sound'`, `create_sound'`.
-/
open Aeneas Aeneas.Std Result H5iAppLib

namespace h5i_app_pgsql.Sound

/-! ## Compiled text is small -/

def NB (n : Pg.Name) : Prop := n.length ≤ 63

def CB : Pg.Cond → Prop
  | .eq n p => NB n ∧ p ≤ 1601
  | .same n p => NB n ∧ p ≤ 1601

/-- Names of at most 63 bytes, at most 1601 of anything, parameters up to `$1601`. -/
def PB : Pg.Sql → Prop
  | .create t cols key => NB t ∧ cols.length ≤ 1601 ∧ (∀ c ∈ cols, NB c.name) ∧ key.length ≤ 1601 ∧ ∀ n ∈ key, NB n
  | .select t cols conds order => NB t ∧ cols.length ≤ 1601 ∧ (∀ n ∈ cols, NB n) ∧ conds.length ≤ 1601 ∧
      (∀ c ∈ conds, CB c) ∧ order.length ≤ 1601 ∧ ∀ n ∈ order, NB n
  | .insert t cols cf up => NB t ∧ cols.length ≤ 1601 ∧ (∀ n ∈ cols, NB n) ∧ cf.length ≤ 1601 ∧
      (∀ n ∈ cf, NB n) ∧ up.length ≤ 1601 ∧ ∀ n ∈ up, NB n
  | .delete t conds => NB t ∧ conds.length ≤ 1601 ∧ ∀ c ∈ conds, CB c

theorem lit_len (s : String) : (Pg.lit s).length = s.toList.length := by simp [Pg.lit]

/-- Lengths of appends and literals. -/
macro "lens" : tactic => `(tactic| simp only [List.length_append, List.length_cons, List.length_nil,
  List.length_singleton, lit_len, String.reduceToList, Char.isValue, zero_add, Nat.reduceAdd] at *)

theorem flatMap_len {α : Type} (l : List α) (f : α → List Nat) (M : Nat) (h : ∀ x ∈ l, (f x).length ≤ M) :
    (l.flatMap f).length ≤ l.length * M := by
  induction l with
  | nil => simp
  | cons x xs ih =>
    simp only [List.flatMap_cons, List.length_append, List.length_cons]
    have := h x List.mem_cons_self
    have := ih (fun y hy => h y (List.mem_cons_of_mem _ hy))
    rw [Nat.succ_mul]; omega

theorem commaSep_len (xs : List (List Nat)) (M : Nat) (h : ∀ x ∈ xs, x.length ≤ M) :
    (Pg.commaSep xs).length ≤ xs.length * (M + 2) := by
  cases xs with
  | nil => simp [Pg.commaSep]
  | cons x xs =>
    simp only [Pg.commaSep, List.length_append, List.length_cons]
    have h1 := h x List.mem_cons_self
    have h2 := flatMap_len xs (fun y => Pg.lit ", " ++ y) (M + 2) (fun y hy => by
      lens; have := h y (List.mem_cons_of_mem _ hy); omega)
    rw [Nat.succ_mul]; omega

theorem quote_len (n : Pg.Name) : (Pg.quote n).length ≤ 2 * n.length + 2 := by
  simp only [Pg.quote, List.length_cons, List.length_append, List.length_singleton, List.length_nil]
  have := flatMap_len n (fun c => if c = 34 then [34, 34] else [c]) 2 (fun c _ => by split <;> simp)
  omega

theorem quote_nb (n : Pg.Name) (h : NB n) : (Pg.quote n).length ≤ 128 := by
  have := quote_len n; unfold NB at h; omega

theorem digits_len (n : Nat) : (Pg.digits n).length ≤ n + 1 := by
  induction n using Nat.strong_induction_on with
  | _ n ih =>
    rw [Pg.digits]; split
    · simp
    · have := ih (n / 10) (by omega); simp; omega

theorem param_len (p : Nat) : (Pg.param p).length ≤ p + 2 := by
  simp only [Pg.param, List.length_cons]; have := digits_len p; omega

theorem cond_len (c : Pg.Cond) (h : CB c) : (Pg.condText c).length ≤ 1760 := by
  cases c with
  | eq n p =>
    obtain ⟨h1, h2⟩ := h
    simp only [Pg.condText, List.length_append]
    have := quote_nb n h1; have := param_len p; lens; omega
  | same n p =>
    obtain ⟨h1, h2⟩ := h
    simp only [Pg.condText, List.length_append]
    have := quote_nb n h1; have := param_len p; lens; omega

theorem names_len (ns : List Pg.Name) (hl : ns.length ≤ 1601) (h : ∀ n ∈ ns, NB n) :
    (Pg.commaSep (ns.map Pg.quote)).length ≤ 1601 * 130 := by
  have := commaSep_len (ns.map Pg.quote) 128 (fun x hx => by
    obtain ⟨n, hn, rfl⟩ := List.mem_map.1 hx; exact quote_nb n (h n hn))
  simp only [List.length_map] at this
  calc _ ≤ ns.length * (128 + 2) := this
    _ ≤ 1601 * 130 := Nat.mul_le_mul_right _ hl

theorem where_len (cs : List Pg.Cond) (hl : cs.length ≤ 1601) (h : ∀ c ∈ cs, CB c) :
    (Pg.whereText cs).length ≤ 1601 * 1767 := by
  cases cs with
  | nil => simp [Pg.whereText]
  | cons c cs =>
    simp only [Pg.whereText, List.length_append]
    have h1 := cond_len c (h c List.mem_cons_self)
    have h2 := flatMap_len cs (fun c => Pg.lit " AND " ++ Pg.condText c) 1767 (fun d hd => by
      lens; have := cond_len d (h d (List.mem_cons_of_mem _ hd)); omega)
    simp at hl
    have : cs.length * 1767 ≤ 1600 * 1767 := Nat.mul_le_mul_right _ (by omega)
    lens; omega

theorem render_len (s : Pg.Sql) (h : PB s) : (Pg.render s).length ≤ 10 ^ 7 := by
  cases s with
  | create t cols key =>
    obtain ⟨h1, h2, h3, h4, h5⟩ := h
    simp only [Pg.render, List.length_append]
    have := quote_nb t h1
    have := names_len key h4 h5
    have hc := flatMap_len cols Pg.colDefText 150 (fun c hc => by
      simp only [Pg.colDefText, List.length_append]
      have := quote_nb c.name (h3 c hc)
      cases c.kind <;> cases c.notNull <;> simp [Pg.kindText, Pg.lit] <;> omega)
    have : cols.length * 150 ≤ 1601 * 150 := Nat.mul_le_mul_right _ h2
    lens; omega
  | select t cols conds order =>
    obtain ⟨h1, h2, h3, h4, h5, h6, h7⟩ := h
    simp only [Pg.render, List.length_append]
    have := quote_nb t h1
    have := names_len cols h2 h3
    have := where_len conds h4 h5
    have := names_len order h6 h7
    split <;> lens <;> omega
  | insert t cols cf up =>
    obtain ⟨h1, h2, h3, h4, h5, h6, h7⟩ := h
    simp only [Pg.render, List.length_append]
    have := quote_nb t h1
    have := names_len cols h2 h3
    have := names_len cf h4 h5
    have hp := commaSep_len ((List.range cols.length).map (fun i => Pg.param (i + 1))) 1603 (fun x hx => by
      obtain ⟨i, hi, rfl⟩ := List.mem_map.1 hx
      have := param_len (i + 1); simp at hi; omega)
    simp only [List.length_map, List.length_range] at hp
    have : cols.length * (1603 + 2) ≤ 1601 * 1605 := Nat.mul_le_mul_right _ h2
    have hs := commaSep_len (up.map (fun n => Pg.quote n ++ Pg.lit " = EXCLUDED." ++ Pg.quote n)) 300 (fun x hx => by
      obtain ⟨n, hn, rfl⟩ := List.mem_map.1 hx
      have := quote_nb n (h7 n hn); lens; omega)
    simp only [List.length_map] at hs
    have : up.length * (300 + 2) ≤ 1601 * 302 := Nat.mul_le_mul_right _ h6
    unfold Pg.placeholders
    split <;> lens <;> omega
  | delete t conds =>
    obtain ⟨h1, h2, h3⟩ := h
    simp only [Pg.render, List.length_append]
    have := quote_nb t h1
    have := where_len conds h2 h3
    lens; omega

/-! ## What the compiler returns is small -/

section Bounds
variable {V : Type} (kind : V → Option Pg.Kind)

theorem tab_facts {tb : Pg.Tab} (h : Pg.TabOk tb) :
    NB tb.name ∧ (∀ n ∈ Pg.names tb, NB n) ∧ (Pg.names tb).length ≤ 1600 ∧ tb.kl ≤ tb.cols.length ∧
      (∀ c ∈ tb.cols, NB c.name) := by
  obtain ⟨h1, -, -, h4, h5, h6, -⟩ := h
  refine ⟨h1.2.1, fun n hn => (h6 n hn).2.1, by rw [Pg.names_length]; omega, h5, fun c hc => ?_⟩
  exact (h6 c.name (List.mem_cons_of_mem _ (List.mem_map_of_mem hc))).2.1

theorem tenant_nb : NB Pg.tenantName := by unfold NB Pg.tenantName; lens; omega

theorem tabOk_at {ts : List Pg.Tab} (hv : Pg.Valid ts) {t : Nat} {tb : Pg.Tab} (ht : ts[t]? = some tb) :
    Pg.TabOk tb := hv.1 tb (List.mem_of_getElem? ht)

theorem take_names (tb : Pg.Tab) (m : Nat) (h : ∀ n ∈ Pg.names tb, NB n) (hl : (Pg.names tb).length ≤ 1600) :
    ((Pg.names tb).take m).length ≤ 1601 ∧ ∀ n ∈ (Pg.names tb).take m, NB n :=
  ⟨by simp; omega, fun n hn => h n (List.mem_of_mem_take hn)⟩

theorem compileA_pb (ts : List Pg.Tab) (tv : V) (s : Sql.AStmt V) (q : Pg.Sql) (ps : List V)
    (hc : Pg.compileA kind ts tv s = some (q, ps)) : PB q := by
  cases s with
  | up t k r =>
    simp only [Pg.compileA] at hc
    split at hc
    · rename_i hv
      cases ht : ts[t]? with
      | none => simp [ht] at hc
      | some tb =>
        simp only [ht] at hc
        split at hc
        · simp only [Option.some.injEq, Prod.mk.injEq] at hc
          obtain ⟨rfl, -⟩ := hc
          obtain ⟨h1, h2, h3, h4, -⟩ := tab_facts (tabOk_at hv ht)
          have hk := take_names tb (tb.kl + 1) h2 h3
          rw [← Pg.keyNames_take] at hk
          have hup : (tb.cols.drop tb.kl).map (·.name) = (Pg.names tb).drop (tb.kl + 1) := by
            simp [Pg.names, List.map_drop]
          refine ⟨h1, by omega, h2, hk.1, hk.2, ?_, ?_⟩
          · rw [hup]; simp; omega
          · rw [hup]; exact fun n hn => h2 n (List.mem_of_mem_drop hn)
        · simp at hc
    · simp at hc
  | del t k =>
    simp only [Pg.compileA] at hc
    split at hc
    · rename_i hv
      cases ht : ts[t]? with
      | none => simp [ht] at hc
      | some tb =>
        simp only [ht] at hc
        split at hc
        · simp only [Option.some.injEq, Prod.mk.injEq] at hc
          obtain ⟨rfl, -⟩ := hc
          obtain ⟨h1, h2, h3, h4, h5⟩ := tab_facts (tabOk_at hv ht)
          refine ⟨h1, ?_, ?_⟩
          · simp; rw [Pg.names_length] at h3; omega
          · intro c hc
            rcases List.mem_cons.1 hc with rfl | hc
            · exact ⟨tenant_nb, by omega⟩
            · obtain ⟨i, hi, rfl⟩ := List.mem_mapIdx.1 hc
              simp at hi
              rw [Pg.names_length] at h3
              exact ⟨h5 _ (List.mem_of_mem_take (List.getElem_mem _)), by omega⟩
        · simp at hc
    · simp at hc
  | delWhere t i v =>
    simp only [Pg.compileA] at hc
    split at hc
    · rename_i hv
      cases ht : ts[t]? with
      | none => simp [ht] at hc
      | some tb =>
        simp only [ht] at hc
        cases hci : tb.cols[i]? with
        | none => simp [hci] at hc
        | some c =>
          simp only [hci] at hc
          split at hc
          · simp only [Option.some.injEq, Prod.mk.injEq] at hc
            obtain ⟨rfl, -⟩ := hc
            obtain ⟨h1, -, -, -, h5⟩ := tab_facts (tabOk_at hv ht)
            refine ⟨h1, by simp, ?_⟩
            intro d hd
            simp only [List.mem_cons, List.not_mem_nil, or_false] at hd
            rcases hd with rfl | rfl
            · exact ⟨tenant_nb, by omega⟩
            · exact ⟨h5 c (List.mem_of_getElem? hci), by omega⟩
          · simp at hc
    · simp at hc

theorem selectA_pb (ts : List Pg.Tab) (tv : V) (t : Nat) (f : Option (Nat × V)) (q : Pg.Sql) (ps : List V)
    (hc : Pg.selectA kind ts tv t f = some (q, ps)) : PB q := by
  simp only [Pg.selectA] at hc
  split at hc
  · rename_i hv
    cases ht : ts[t]? with
    | none => simp [ht] at hc
    | some tb =>
      simp only [ht] at hc
      obtain ⟨h1, h2, h3, h4, h5⟩ := tab_facts (tabOk_at hv ht)
      rw [Pg.names_length] at h3
      have hcols : (tb.cols.map (·.name)).length ≤ 1601 ∧ ∀ n ∈ tb.cols.map (·.name), NB n :=
        ⟨by simp; omega, fun n hn => by obtain ⟨c, hc, rfl⟩ := List.mem_map.1 hn; exact h5 c hc⟩
      have hord : ((tb.cols.take tb.kl).map (·.name)).length ≤ 1601 ∧
          ∀ n ∈ (tb.cols.take tb.kl).map (·.name), NB n :=
        ⟨by simp; omega, fun n hn => by
          obtain ⟨c, hc, rfl⟩ := List.mem_map.1 hn; exact h5 c (List.mem_of_mem_take hc)⟩
      cases f with
      | none =>
        simp only [Option.some.injEq, Prod.mk.injEq] at hc
        obtain ⟨rfl, -⟩ := hc
        refine ⟨h1, hcols.1, hcols.2, by simp, ?_, hord.1, hord.2⟩
        intro c hc; simp at hc; subst hc; exact ⟨tenant_nb, by omega⟩
      | some p =>
        obtain ⟨i, v⟩ := p
        cases hci : tb.cols[i]? with
        | none => simp [hci] at hc
        | some c =>
          simp only [hci] at hc
          split at hc
          · simp only [Option.some.injEq, Prod.mk.injEq] at hc
            obtain ⟨rfl, -⟩ := hc
            refine ⟨h1, hcols.1, hcols.2, by simp, ?_, hord.1, hord.2⟩
            intro d hd
            simp only [List.mem_cons, List.not_mem_nil, or_false] at hd
            rcases hd with rfl | rfl
            · exact ⟨tenant_nb, by omega⟩
            · exact ⟨h5 c (List.mem_of_getElem? hci), by omega⟩
          · simp at hc
  · simp at hc

theorem createA_pb (tb : Pg.Tab) (h : Pg.TabOk tb) : PB (Pg.createA tb) := by
  obtain ⟨h1, h2, h3, -, -⟩ := tab_facts h
  have hk := take_names tb (tb.kl + 1) h2 h3
  rw [← Pg.keyNames_take] at hk
  refine ⟨h1, ?_, fun c hc => h2 c.name (by rw [← Pg.colDefs_names]; exact List.mem_map_of_mem hc), hk.1, hk.2⟩
  rw [← List.length_map (f := (·.name)), Pg.colDefs_names]; omega

end Bounds

theorem small_of (s : Sql) (h : PB s.abs) : Small s := by
  refine ⟨le_trans (render_len _ h) (by scalar_tac), ?_⟩
  cases s with
  | Insert t cols cf up =>
    have := h.2.1
    simp only [Bs, List.length_map] at this
    show cols.val.length ≤ U32.max
    scalar_tac
  | _ => trivial

/-! ## The extracted compiler, end to end -/

theorem compileA_valid {V : Type} (kind : V → Option Pg.Kind) (ts : List Pg.Tab) (tv : V) (s : Sql.AStmt V)
    (q : Pg.Sql × List V) (hc : Pg.compileA kind ts tv s = some q) : Pg.Valid ts := by
  by_contra hv
  cases s <;> simp [Pg.compileA, hv] at hc

theorem selectA_valid {V : Type} (kind : V → Option Pg.Kind) (ts : List Pg.Tab) (tv : V) (t : Nat)
    (f : Option (Nat × V)) (q : Pg.Sql × List V) (hc : Pg.selectA kind ts tv t f = some q) : Pg.Valid ts := by
  by_contra hv
  simp [Pg.selectA, hv] at hc

/-- `compile` output prints as `Pg.render`, runs as `exec` on the tenant, touches nothing else. -/
theorem write_sound (ts : alloc.vec.Vec Table) (tenant : I64) (s : h5i_app_sql.Stmt) (q : Query)
    (hc : compile ts tenant s = ok (some q)) (db : Pg.PgDb h5i_app_sql.Val) (hm : Pg.Matches valKind (tabs ts) db) :
    render q.sql ⦃ b => B b = Pg.render q.sql.abs ⦄ ∧
      ∃ db', Pg.run valKind db q.sql.abs q.params.val = some db' ∧ Pg.Matches valKind (tabs ts) db' ∧
        Pg.view (tabs ts) (.Int tenant) db' = H5iAppLib.Sql.exec (Pg.view (tabs ts) (.Int tenant) db) (Stmt.abs s) ∧
        (∀ tv, tv ≠ .Int tenant → Pg.view (tabs ts) tv db' = Pg.view (tabs ts) tv db) ∧
        ∀ n, n ∉ (tabs ts).map (·.name) → db' n = db n := by
  have hq := post_of_ok (Comp.compile_spec ts tenant s) hc
  simp only [Option.map_some] at hq
  have hq' : Pg.compileA valKind (tabs ts) (.Int tenant) (Stmt.abs s) = some (q.sql.abs, q.params.val) := hq.symm
  have hv := compileA_valid valKind _ _ _ _ hq'
  refine ⟨render_spec _ (small_of _ (compileA_pb valKind _ _ _ _ _ hq')), ?_⟩
  exact Pg.compile_sound valKind hv rfl hm hq'

/-- `select` output prints as `Pg.render` and returns the tenant's matching rows once each. -/
theorem select_sound' (ts : alloc.vec.Vec Table) (tenant : I64) (table : U32) (filter : Option (U32 × h5i_app_sql.Val))
    (q : Query) (hc : select ts tenant table filter = ok (some q)) (db : Pg.PgDb h5i_app_sql.Val)
    (hm : Pg.Matches valKind (tabs ts) db) :
    render q.sql ⦃ b => B b = Pg.render q.sql.abs ⦄ ∧
      ∃ R0, Pg.selected valKind db q.sql.abs q.params.val = some R0 ∧
        ∀ R, R.Perm R0 → H5iAppLib.Sql.Sel (Pg.klOf (tabs ts)) (Pg.view (tabs ts) (.Int tenant) db) table.val
          (Pg.Filter (filter.map (fun p => (p.1.val, p.2)))) R := by
  have hq := post_of_ok (Comp.select_spec ts tenant table filter) hc
  simp only [Option.map_some] at hq
  have hq' := hq.symm
  have hv := selectA_valid valKind _ _ _ _ _ hq'
  exact ⟨render_spec _ (small_of _ (selectA_pb valKind _ _ _ _ _ _ hq')), Pg.select_sound valKind hv rfl hm hq'⟩

/-- On a valid schema, `create` makes the tables `Pg.Matches` expects (empty),
and is a no-op where they already match. -/
theorem create_sound' (ts : alloc.vec.Vec Table) (hv : valid ts = ok true) :
    (∀ i (hi : i < ts.length), create (ts.val[i]'(by simpa using hi)) ⦃ s =>
        s.abs = Pg.createA (ts.val[i]'(by simpa using hi)).abs ∧ render s ⦃ b => B b = Pg.render s.abs ⦄ ⦄) ∧
      (∀ db : Pg.PgDb h5i_app_sql.Val, (∀ tb ∈ tabs ts, db tb.name = none) →
        ∃ db', Pg.runAll valKind db ((tabs ts).map (fun tb => (Pg.createA tb, []))) = some db' ∧
          Pg.Matches valKind (tabs ts) db' ∧ ∀ tv, Pg.view (tabs ts) tv db' = fun _ _ => none) ∧
      (∀ db : Pg.PgDb h5i_app_sql.Val, Pg.Matches valKind (tabs ts) db →
        Pg.runAll valKind db ((tabs ts).map (fun tb => (Pg.createA tb, []))) = some db) := by
  have hV : Pg.Valid (tabs ts) := (post_of_ok (Comp.valid_spec ts) hv).1 rfl
  refine ⟨fun i hi => ?_, fun db hn => ?_, fun db hm => ?_⟩
  · have hok := hV.1 _ (List.mem_of_getElem? (Comp.tabs_get ts i hi))
    have hc : (ts.val[i]'(by simpa using hi)).columns.length < 1600 := by
      have := hok.2.2.2.1; rwa [Comp.abs_cols_length] at this
    apply WP.spec_mono (Comp.create_spec _ hc)
    intro r hr
    refine ⟨hr, render_spec _ (small_of _ ?_)⟩
    rw [hr]; exact createA_pb _ hok
  · obtain ⟨db', h1, h2, h3, -⟩ := Pg.create_fresh valKind hV db hn
    exact ⟨db', h1, h2, h3⟩
  · exact Pg.create_kept valKind _ db (fun tb htb => by
      obtain ⟨tab, h, -⟩ := hm tb htb; simp [h])

end h5i_app_pgsql.Sound
