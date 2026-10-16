import H5iAppLib.Sql
import H5iAppLib.Lists
/-!
# Declarative reads and what they mean

What `h5i_app_sql::Read` and `h5i_app_sql::Pred` mean, over the tables of one
tenant listed in ascending key order (the order `load` returns them in). A
read keeps the rows of one table that satisfy a predicate (column equality,
membership in a column of another read, `and`, `or`), then a window of them:
`offset` and `limit` counted in ascending or descending key order. Its
result is still in ascending key order, a sublist of the table.

`h5i-app-pgsql`'s `compile_read` turns a read into one `SELECT`; that
compilation is differential-tested against `Read.rows` (see
`crates/h5i-app-pg/src/reads_test.rs` and `H5iAppLibTests/QueryCases.lean`),
not proved. The lemmas here are what an app's frame theorem uses to show that
`transition` gives the same result on the loaded slice as on the whole tenant.
-/

namespace H5iAppLib.Query

open H5iAppLib.Sql

/-- Which part of the matching rows a read keeps. -/
structure Window where
  /-- Count `offset` and `limit` from the largest key down. -/
  desc : Bool := false
  offset : Nat := 0
  limit : Option Nat := none
deriving DecidableEq, Repr

/-- `offset` rows dropped, then at most `limit` kept. -/
def cut {α : Type} (o : Nat) (lim : Option Nat) (l : List α) : List α :=
  match lim with
  | none => l.drop o
  | some n => (l.drop o).take n

/-- The window of `l`, a list in ascending key order; ascending again. -/
def Window.apply {α : Type} (w : Window) (l : List α) : List α :=
  if w.desc then (cut w.offset w.limit l.reverse).reverse else cut w.offset w.limit l

inductive Pred (V : Type) where
  /-- Every row. -/
  | all
  /-- Column `i` is `v` (`IS NOT DISTINCT FROM`). -/
  | eq (i : Nat) (v : V)
  /-- Column `i` is the column `j` of some row of the read of table `t` by
  `p` and `w`. Only for columns that are never `NULL`. -/
  | isIn (i t : Nat) (p : Pred V) (j : Nat) (w : Window)
  | and (p q : Pred V)
  | or (p q : Pred V)

structure Read (V : Type) where
  table : Nat
  pred : Pred V
  window : Window := {}

section
variable {V : Type} [DecidableEq V]

/-- `p` holds of `row`, a row of some table of `tabs`. -/
def Pred.holds (tabs : Tables V) : Pred V → List V → Bool
  | .all, _ => true
  | .eq i v, r => decide (r[i]? = some v)
  | .isIn i t p j w, r =>
    match r[i]? with
    | none => false
    | some x => (w.apply ((tabs t).filter (p.holds tabs))).any (fun y => decide (y[j]? = some x))
  | .and p q, r => p.holds tabs r && q.holds tabs r
  | .or p q, r => p.holds tabs r || q.holds tabs r

/-- The rows a read returns, in ascending key order. -/
def Read.rows (tabs : Tables V) (q : Read V) : List (List V) :=
  q.window.apply ((tabs q.table).filter (q.pred.holds tabs))

/-- What `SELECT count(*)` of the read returns. -/
def Read.count (tabs : Tables V) (q : Read V) : Nat := (q.rows tabs).length

/-- The tables a plan loads: each table its read's rows, or none if the plan
has no read of it. -/
def load (plan : List (Read V)) (tabs : Tables V) : Tables V := fun t =>
  match plan.find? (fun q => decide (q.table = t)) with
  | some q => q.rows tabs
  | none => []

end

/-! ## Windows -/

section Windows
variable {α β : Type}

theorem cut_map (o : Nat) (lim : Option Nat) (f : α → β) (l : List α) :
    cut o lim (l.map f) = (cut o lim l).map f := by
  cases lim <;> simp [cut, List.map_drop, List.map_take]

theorem Window.apply_map (w : Window) (f : α → β) (l : List α) : w.apply (l.map f) = (w.apply l).map f := by
  unfold Window.apply; split <;> simp [← cut_map, List.map_reverse]

theorem cut_sublist (o : Nat) (lim : Option Nat) (l : List α) : List.Sublist (cut o lim l) l := by
  cases lim with
  | none => exact List.drop_sublist _ _
  | some n => exact (List.take_sublist _ _).trans (List.drop_sublist _ _)

theorem Window.apply_sublist (w : Window) (l : List α) : List.Sublist (w.apply l) l := by
  unfold Window.apply; split
  · simpa using (cut_sublist w.offset w.limit l.reverse).reverse
  · exact cut_sublist _ _ _

theorem Window.mem_apply {w : Window} {l : List α} {x : α} (h : x ∈ w.apply l) : x ∈ l :=
  (w.apply_sublist l).subset h

@[simp] theorem Window.apply_default (l : List α) : ({} : Window).apply l = l := by
  simp [Window.apply, cut]

/-- The newest `n` rows, read back newest first and paged: the same page as
from the whole list, as long as `n` covers `offset + limit`. -/
theorem page_of_newest (l : List α) (o n : Nat) :
    (((Window.apply { desc := true, limit := some (o + n) } l).reverse.drop o).take n) = (l.reverse.drop o).take n := by
  simp only [Window.apply, cut, if_true, List.drop_zero, List.reverse_reverse]
  rw [List.drop_take, List.take_take, Nat.min_def]
  split <;> [rfl; omega]

end Windows

/-! ## Reads over encoded rows -/

section Enc
variable {V : Type} [DecidableEq V] {α : Type}

omit [DecidableEq V] in
theorem filter_map_holds (f : α → List V) (P : List V → Bool) (l : List α) :
    (l.map f).filter P = (l.filter (fun x => P (f x))).map f := by
  rw [List.filter_map]; rfl

/-- A read of a table that encodes the list `l` returns the encoding of a
window of a filter of `l`. -/
theorem rows_enc (tabs : Tables V) (q : Read V) (f : α → List V) (l : List α) (h : tabs q.table = l.map f) :
    q.rows tabs = (q.window.apply (l.filter (fun x => q.pred.holds tabs (f x)))).map f := by
  rw [Read.rows, h, filter_map_holds, Window.apply_map]

/-- `isIn` against an encoded table, in terms of the decoded rows. -/
theorem holds_isIn (tabs : Tables V) (i t j : Nat) (p : Pred V) (w : Window) (r : List V) (f : α → List V)
    (l : List α) (h : tabs t = l.map f) :
    (Pred.isIn i t p j w).holds tabs r =
      match r[i]? with
      | none => false
      | some x => (w.apply (l.filter (fun y => p.holds tabs (f y)))).any (fun y => decide ((f y)[j]? = some x)) := by
  simp only [Pred.holds, h, filter_map_holds, Window.apply_map, List.any_map, Function.comp_def]

end Enc

/-! ## What a plan loads, table by table -/

section Pick
variable {V : Type} [DecidableEq V] {α : Type}

/-- The rows of table `t` (the list `l`, encoded by `row`) that `plan`
loads from `tabs`: its read's window of the rows its predicate keeps, or
none if the plan has no read of `t`. -/
def pickRows (plan : List (Read V)) (tabs : Tables V) (t : Nat) (row : α → List V) (l : List α) : List α :=
  match plan.find? (fun q => decide (q.table = t)) with
  | some q => q.window.apply (l.filter (fun x => q.pred.holds tabs (row x)))
  | none => []

theorem pickRows_sublist (plan : List (Read V)) (tabs : Tables V) (t : Nat) (row : α → List V) (l : List α) :
    List.Sublist (pickRows plan tabs t row l) l := by
  unfold pickRows; split
  · exact (Window.apply_sublist _ _).trans List.filter_sublist
  · exact List.nil_sublist _

theorem pickRows_length (plan : List (Read V)) (tabs : Tables V) (t : Nat) (row : α → List V) (l : List α) :
    (pickRows plan tabs t row l).length ≤ l.length :=
  (pickRows_sublist plan tabs t row l).length_le

/-- `load` of a table that encodes `l` is the encoding of what `pickRows` keeps. -/
theorem load_pick (plan : List (Read V)) (tabs : Tables V) (t : Nat) (row : α → List V) (l : List α)
    (h : tabs t = l.map row) : load plan tabs t = (pickRows plan tabs t row l).map row := by
  unfold load pickRows
  cases hq : plan.find? (fun q => decide (q.table = t)) with
  | none => rfl
  | some q =>
    have ht : q.table = t := by simpa using (List.find?_some hq)
    exact rows_enc tabs q row l (by rw [ht]; exact h)

/-- A read without a window, as a filter. -/
theorem pickRows_filter (plan : List (Read V)) (tabs : Tables V) (t : Nat) (row : α → List V) (l : List α)
    (q : Read V) (hq : plan.find? (fun q => decide (q.table = t)) = some q) (hw : q.window = {}) :
    pickRows plan tabs t row l = l.filter (fun x => q.pred.holds tabs (row x)) := by
  simp [pickRows, hq, hw]

end Pick

/-! ## Lookups on a slice -/

section Lookups
variable {α : Type}

/-- A slice that keeps every row a lookup can return answers the lookup like
the whole table. The frame lemma behind key-scoped loads. -/
theorem find?_slice (l : List α) (p keep : α → Bool) (h : ∀ x ∈ l, p x = true → keep x = true) :
    (l.filter keep).find? p = l.find? p :=
  find?_filter_of_imp l p keep h

/-- Likewise for an `any` (an existence check). -/
theorem any_slice (l : List α) (p keep : α → Bool) (h : ∀ x ∈ l, p x = true → keep x = true) :
    (l.filter keep).any p = l.any p := by
  apply Bool.eq_iff_iff.2
  simp only [List.any_eq_true, List.mem_filter]
  exact ⟨fun ⟨x, ⟨hx, _⟩, hp⟩ => ⟨x, hx, hp⟩, fun ⟨x, hx, hp⟩ => ⟨x, ⟨hx, h x hx hp⟩, hp⟩⟩

/-- And for the rows a filter selects (a count, the rows of one key). -/
theorem filter_slice (l : List α) (p keep : α → Bool) (h : ∀ x ∈ l, p x = true → keep x = true) :
    (l.filter keep).filter p = l.filter p := by
  rw [List.filter_filter]
  apply List.filter_congr
  intro x hx
  cases hp : p x <;> simp [hp, h x hx]

end Lookups

end H5iAppLib.Query
