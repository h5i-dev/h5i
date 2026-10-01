import H5iAppLib.Tables
/-!
# Keyed statements and what they mean

Generic over the column value type, so `h5i-app-sql` (its own extraction) and
each app kernel (which extracts `h5i_app_sql` again) share one semantics.
-/

namespace H5iAppLib.Sql

open Classical

variable {V : Type}

inductive AStmt (V : Type) where
  | up (t : Nat) (key rest : List V)
  | del (t : Nat) (key : List V)
  /-- Delete every row whose column `i` is `v`. -/
  | delWhere (t i : Nat) (v : V)

inductive AWrite (V : Type) where
  | put (t kl : Nat) (row : List V)
  | del (t : Nat) (key : List V)
  | delWhere (t i : Nat) (v : V)

/-- The plan, over lists: split a put's row into key and rest. -/
def planA : AWrite V → AStmt V
  | .put t kl row => .up t (row.take kl) (row.drop kl)
  | .del t k => .del t k
  | .delWhere t i v => .delWhere t i v

/-- One tenant's database: the row stored at each (table, key), if any. -/
def Db (V : Type) := Nat → List V → Option (List V)

/-- What a statement does to one tenant's rows. `H5iAppLib.Pg.compile_sound`
proves that the SQL `h5i-app-pgsql` compiles a statement to does exactly this,
under the PostgreSQL model in `H5iAppLib.Pg`. -/
noncomputable def exec (db : Db V) : AStmt V → Db V
  | .up t k r => fun t' k' => if t' = t ∧ k' = k then some (k ++ r) else db t' k'
  | .del t k => fun t' k' => if t' = t ∧ k' = k then none else db t' k'
  | .delWhere t i v => fun t' k' => (db t' k').filter (fun r => !decide (t' = t ∧ r[i]? = some v))

noncomputable def execAll (db : Db V) (ss : List (AStmt V)) : Db V := ss.foldl exec db

/-- Rows per table. -/
def Tables (V : Type) := Nat → List (List V)

section
variable (kl : Nat → Nat)

/-- Apply one write; `kl t` is table `t`'s key length. -/
noncomputable def applyW (tabs : Tables V) : AWrite V → Tables V
  | .put t _ row => fun t' => if t' = t then upsert (·.take (kl t)) row (tabs t) else tabs t'
  | .del t k => fun t' => if t' = t then (tabs t).filter (fun r => ¬ r.take (kl t) = k) else tabs t'
  | .delWhere t i v => fun t' => if t' = t then (tabs t).filter (fun r => ¬ r[i]? = some v) else tabs t'

noncomputable def applyAllW (tabs : Tables V) (ws : List (AWrite V)) : Tables V := ws.foldl (applyW kl) tabs

/-- The database a set of keyed tables stands for. Row order does not matter. -/
noncomputable def readBack (tabs : Tables V) : Db V :=
  fun t k => (tabs t).find? (fun r => r.take (kl t) = k)

/-- Keys are unique and every row has a full key. -/
def WellKeyed (tabs : Tables V) : Prop :=
  ∀ t, ((tabs t).map (·.take (kl t))).Nodup ∧ ∀ r ∈ tabs t, kl t ≤ r.length

/-- A put uses its table's key length. Deletes are on keyed tables: a table
without key columns holds one row, which is only ever replaced. -/
def WriteOk : AWrite V → Prop
  | .put t n row => n = kl t ∧ kl t ≤ row.length
  | .del t _ => 0 < kl t
  | .delWhere t _ _ => 0 < kl t
end

theorem find_upsert (n : Nat) (row : List V) (k : List V) (rs : List (List V)) :
    (upsert (·.take n) row rs).find? (fun r => r.take n = k) =
      if row.take n = k then some row else rs.find? (fun r => r.take n = k) := by
  induction rs with
  | nil => simp [upsert]
  | cons r rs ih =>
    unfold upsert
    by_cases hr : r.take n = row.take n
    · simp only [hr, if_true, List.find?_cons]
      by_cases hk : row.take n = k <;> simp_all
    · simp only [hr, if_false, List.find?_cons, ih]
      by_cases hk : row.take n = k <;> by_cases hk' : r.take n = k <;> simp_all

theorem keys_upsert (n : Nat) (row : List V) (rs : List (List V)) :
    (upsert (·.take n) row rs).map (·.take n) =
      if row.take n ∈ rs.map (·.take n) then rs.map (·.take n) else rs.map (·.take n) ++ [row.take n] := by
  induction rs with
  | nil => simp [upsert]
  | cons r rs ih =>
    unfold upsert
    by_cases hr : r.take n = row.take n
    · simp [hr]
    · simp only [hr, if_false, List.map_cons, ih, List.mem_cons]
      by_cases hm : row.take n ∈ rs.map (·.take n)
      · simp [hm]
      · have : ¬ row.take n = r.take n := fun h => hr h.symm
        simp [hm, this]

theorem find_filter_ne (n : Nat) (k k' : List V) (h : k' ≠ k) (rs : List (List V)) :
    (rs.filter (fun r => ¬ r.take n = k)).find? (fun r => r.take n = k') =
      rs.find? (fun r => r.take n = k') := by
  simp only [List.find?_filter]
  congr 1
  funext r
  by_cases hr : r.take n = k' <;> simp_all

theorem find_filter_eq (n : Nat) (k : List V) (rs : List (List V)) :
    (rs.filter (fun r => ¬ r.take n = k)).find? (fun r => r.take n = k) = none := by
  rw [List.find?_eq_none]
  intro x hx
  simp only [List.mem_filter, decide_eq_true_eq] at hx
  simpa using hx.2

/-- With unique keys, looking a key up after filtering is filtering the
lookup. -/
theorem find_filter_unique (n : Nat) (q : List V → Prop) [DecidablePred q] (rs : List (List V))
    (hn : (rs.map (·.take n)).Nodup) (k : List V) :
    (rs.filter (fun r => ¬ q r)).find? (fun r => r.take n = k) =
      (rs.find? (fun r => r.take n = k)).filter (fun r => !decide (q r)) := by
  induction rs with
  | nil => simp
  | cons r rs ih =>
    simp only [List.map_cons, List.nodup_cons, List.mem_map] at hn
    by_cases hr : r.take n = k
    · have hnone : (rs.filter (fun r => ¬ q r)).find? (fun r => r.take n = k) = none := by
        rw [List.find?_eq_none]
        intro x hx
        have := (List.mem_filter.1 hx).1
        simp only [decide_eq_true_eq]
        intro hx'
        exact hn.1 ⟨x, this, hx'.trans hr.symm⟩
      by_cases hq : q r
      · simp only [List.filter_cons, hq, not_true_eq_false, decide_false, Bool.false_eq_true,
          if_false, hnone, List.find?_cons, hr, decide_true, Option.filter_some, Bool.not_true]
      · simp only [List.filter_cons, hq, not_false_eq_true, decide_true, if_true, List.find?_cons, hr,
          Option.filter_some, decide_false, Bool.not_false]
    · have ih' := ih hn.2
      by_cases hq : q r
      · simp only [List.filter_cons, hq, not_true_eq_false, decide_false, Bool.false_eq_true,
          if_false, List.find?_cons, hr]
        exact ih'
      · simp only [List.filter_cons, hq, not_false_eq_true, decide_true, if_true, List.find?_cons, hr,
          decide_false]
        exact ih'

theorem step_sound (kl : Nat → Nat) (tabs : Tables V) (w : AWrite V)
    (hk : WellKeyed kl tabs) (hw : WriteOk kl w) :
    readBack kl (applyW kl tabs w) = exec (readBack kl tabs) (planA w) ∧
    WellKeyed kl (applyW kl tabs w) := by
  cases w with
  | put t n row =>
    obtain ⟨rfl, hlen⟩ := hw
    refine ⟨?_, ?_⟩
    · funext t' k'
      simp only [readBack, applyW, exec, planA, List.take_append_drop]
      by_cases ht : t' = t
      · subst ht
        simp only [if_true, true_and, find_upsert]
        by_cases hk' : k' = row.take (kl t') <;> simp_all [eq_comm]
      · simp [ht]
    · intro t'
      simp only [applyW]
      by_cases ht : t' = t
      · subst ht
        simp only [if_true, keys_upsert]
        refine ⟨?_, fun r hr => ?_⟩
        · split
          · exact (hk t').1
          · rename_i hm
            exact List.nodup_append.2 ⟨(hk t').1, by simp, fun a ha b hb hab =>
              hm (by rw [List.mem_singleton] at hb; subst hb; subst hab; exact ha)⟩
        · rcases (mem_upsert_iff (hk t').1).1 hr with rfl | ⟨hr, _⟩
          · exact hlen
          · exact (hk t').2 r hr
      · simpa [ht] using hk t'
  | del t k =>
    refine ⟨?_, ?_⟩
    · funext t' k'
      simp only [readBack, applyW, exec, planA]
      by_cases ht : t' = t
      · subst ht
        by_cases hk' : k' = k
        · subst hk'
          simp only [if_true, and_self]
          exact find_filter_eq _ _ _
        · simp only [if_true, true_and, hk', if_false]
          exact find_filter_ne _ _ _ hk' _
      · simp [ht]
    · intro t'
      simp only [applyW]
      by_cases ht : t' = t
      · subst ht
        simp only [if_true]
        refine ⟨?_, fun r hr => (hk t').2 r (List.mem_filter.1 hr).1⟩
        exact (hk t').1.sublist (List.Sublist.map _ List.filter_sublist)
      · simpa [ht] using hk t'
  | delWhere t i v =>
    refine ⟨?_, ?_⟩
    · funext t' k'
      simp only [readBack, applyW, exec, planA]
      by_cases ht : t' = t
      · subst ht
        simp only [if_true, true_and]
        exact find_filter_unique _ _ _ (hk t').1 _
      · simp only [ht, if_false, false_and, decide_false, Bool.not_false]
        cases (tabs t').find? (fun r => decide (r.take (kl t') = k')) <;> rfl
    · intro t'
      simp only [applyW]
      by_cases ht : t' = t
      · subst ht
        simp only [if_true]
        refine ⟨?_, fun r hr => (hk t').2 r (List.mem_filter.1 hr).1⟩
        exact (hk t').1.sublist (List.Sublist.map _ List.filter_sublist)
      · simpa [ht] using hk t'

/-- Running the plan on the database equals applying the writes to keyed
tables, and keys stay unique. -/
theorem plan_sound (kl : Nat → Nat) (ws : List (AWrite V)) :
    ∀ tabs, WellKeyed kl tabs → (∀ w ∈ ws, WriteOk kl w) →
      readBack kl (applyAllW kl tabs ws) = execAll (readBack kl tabs) (ws.map planA) ∧
      WellKeyed kl (applyAllW kl tabs ws) := by
  induction ws with
  | nil => intro tabs hk _; exact ⟨rfl, hk⟩
  | cons w ws ih =>
    intro tabs hk hw
    obtain ⟨h1, h2⟩ := step_sound kl tabs w hk (hw w (by simp))
    obtain ⟨h3, h4⟩ := ih (applyW kl tabs w) h2 (fun w' hw' => hw w' (by simp [hw']))
    refine ⟨?_, h4⟩
    simp only [applyAllW, List.foldl_cons, execAll, List.map_cons] at h3 ⊢
    rw [h3, h1]

/-! ## Reading rows back, and running a delete by column value -/

section Store
variable (kl : Nat → Nat)

/-- `R` lists every stored row of table `t` satisfying `Q`, once, in any
order: what a compiled filtered `SELECT` returns (`H5iAppLib.Pg.select_sound`). -/
def Sel (db : Db V) (t : Nat) (Q : List V → Prop) (R : List (List V)) : Prop :=
  R.Nodup ∧ ∀ row, row ∈ R ↔ db t (row.take (kl t)) = some row ∧ Q row

/-- `R t` lists every stored row of table `t`, once, in any order: what a
compiled `SELECT` of each table returns (`H5iAppLib.Pg.lists_of_selects`). -/
def Lists (db : Db V) (R : Tables V) : Prop :=
  ∀ t, (R t).Nodup ∧ ∀ row, row ∈ R t ↔ db t (row.take (kl t)) = some row

/-- Column `i` holds `v`. -/
def ColIs (i : Nat) (v : V) (row : List V) : Prop := row[i]? = some v

end Store

end H5iAppLib.Sql
