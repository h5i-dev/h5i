import Proofs
/-!
# What the store holds

The server stores a write set by running the plan of `sql_writes`, and loads
rows through `decode`. `H5iAppLib.Store` proves, for any schema, that the database
then holds what committing computes. Here a write is at most one memory.
-/
open Aeneas Aeneas.Std Result calculator_kernel calculator_kernel.Schema H5iAppLib H5iAppLib.Sql H5iAppLib.Store

namespace calculator_kernel.Storage

/-- The memories' rows. -/
def enc (l : List Memory) : Tables Val
  | 0 => l.map Memory.row
  | _ => []

/-- The table writes of a write set. -/
def sqlA : Option Memory → List (AWrite Val)
  | none => []
  | some m => [Memory.putA m]

/-- What committing a write set does to the memories, as `apply_spec` says. -/
def step (l : List Memory) : Option Memory → List Memory
  | none => l
  | some m => upsert (·.user) m l

def app : App (List Memory) (Option Memory) Val where
  kl := kl
  enc := enc
  sql := sqlA
  step := step
  init := []
  IsRow := IsRow
  enc_step := by schema_step [enc, sqlA, step]
  sql_ok := by schema_ok [sqlA]
  init_ok := by schema_init [enc]
  init_rows := by schema_rows [enc]

theorem fits : Fits app (fun s => s.memories.val) where
  kl := rfl
  rows := rfl
  enc s := by funext t; cases_table t <;> rfl
  init := by funext t; cases_table t <;> rfl
  nil s t h := by cases_table t <;> first | omega | rfl

theorem sql_writes_spec (w : Option Memory) : sql_writes w ⦃ v => v.val.map sqlW = sqlA w ⦄ := by
  unfold sql_writes; cases w <;> step* <;> simp_all [sqlA]

/-- Databases the server produces from an empty tenant, with their memories.
Each request loads rows (`Lists`), decodes them, and runs `sql_writes`'s plan. -/
inductive Served : Db Val → List Memory → Prop
  | fresh : Served (fun _ _ => none) []
  | commit {db : Db Val} {l : List Memory} {r : Rows} {snap : Snapshot} {w : Option Memory}
      {v : alloc.vec.Vec h5i_app_sql.Write} :
      Served db l → Lists kl db (Rows.tabs r) → decode r = ok (some snap) → sql_writes w = ok v →
      Served (execAll db ((v.val.map sqlW).map planA)) (step snap.memories.val w)

theorem served_app {db : Db Val} {l : List Memory} (h : Served db l) : app.Served db l := by
  induction h with
  | fresh => exact .fresh
  | @commit db l r snap w v _ hl hd hv ih =>
    obtain ⟨snap', he, hq⟩ := post_of_ok (loaded fits ih _ hl) hd
    cases he
    have e : (v.val.map sqlW).map planA = (app.sqlAll [w]).map planA := by
      rw [post_of_ok (sql_writes_spec w) hv]; simp [App.sqlAll, app]
    rw [e]
    exact .commit (ws := [w]) ih hq

/-- The store holds what `apply` computes: the rows of the memories its
commits computed, and a load decodes to them, up to row order. -/
theorem stored {db : Db Val} {l : List Memory} (h : Served db l) :
    app.Holds db (enc l) ∧
      ∀ r, Lists kl db (Rows.tabs r) → decode r ⦃ o => ∃ snap, o = some snap ∧ app.Equiv snap.memories.val l ⦄ :=
  ⟨(app.served_holds (served_app h)).1, loaded fits (served_app h)⟩

/-! ## On PostgreSQL

The same requests as the SQL the server sends to a database of all tenants:
`Pg.selectA` (extracted `h5i_app_pgsql::select`) and `Pg.compileA` (extracted
`h5i_app_pgsql::compile`), run on the model in `H5iAppLib.Pg`. -/

/-- How the PostgreSQL driver types a value; `none` is `NULL`. -/
def valKind : Val → Option Pg.Kind
  | .Int _ => some .int
  | .Bool _ => some .bool
  | .Text _ => some .text
  | .Bytes _ => some .bytes
  | .Null => none

section
variable (ts : List Pg.Tab) (tv : Val)

/-- A load of the one table by its compiled `SELECT`, rows in any order. -/
def Loads (db : Pg.PgDb Val) (r : Rows) : Prop :=
  ∃ q ps R0, Pg.selectA valKind ts tv 0 none = some (q, ps) ∧ Pg.selected valKind db q ps = some R0 ∧
    (Rows.tabs r 0).Perm R0

inductive PgServed : Pg.PgDb Val → List Memory → Prop
  | fresh {db : Pg.PgDb Val} :
      Pg.Matches valKind ts db → Pg.view ts tv db = (fun _ _ => none) → PgServed db []
  | commit {db db' : Pg.PgDb Val} {l : List Memory} {r : Rows} {snap : Snapshot} {w : Option Memory}
      {v : alloc.vec.Vec h5i_app_sql.Write} {qs : List (Pg.Sql × List Val)} :
      PgServed db l → Loads ts tv db r → decode r = ok (some snap) → sql_writes w = ok v →
      ((v.val.map sqlW).map planA).mapM (Pg.compileA valKind ts tv) = some qs →
      Pg.runAll valKind db qs = some db' → PgServed db' (step snap.memories.val w)
  | other {db db' : Pg.PgDb Val} {l : List Memory} {tv' : Val} {ss : List (AStmt Val)}
      {qs : List (Pg.Sql × List Val)} :
      PgServed db l → tv' ≠ tv → valKind tv' = some .int →
      ss.mapM (Pg.compileA valKind ts tv') = some qs → Pg.runAll valKind db qs = some db' → PgServed db' l
end

variable {ts : List Pg.Tab} {tv : Val}

theorem lists_of (hv : Pg.Valid ts) (htv : valKind tv = some .int) (hkl : Pg.klOf ts = kl)
    (hlen : ts.length = 1) {db : Pg.PgDb Val} (hm : Pg.Matches valKind ts db) {r : Rows}
    (h : Loads ts tv db r) : Lists kl (Pg.view ts tv db) (Rows.tabs r) := by
  rw [← hkl]
  refine Pg.lists_of_selects valKind hv htv hm _ (fun t ht => ?_) (fun t ht => ?_)
  · obtain ⟨k, rfl⟩ := Nat.exists_eq_add_of_le' (hlen ▸ ht : 1 ≤ t)
    simp only [Rows.tabs]
  · have : t = 0 := by omega
    subst this
    exact h

theorem pg_served (hv : Pg.Valid ts) (htv : valKind tv = some .int) (hkl : Pg.klOf ts = kl)
    (hlen : ts.length = 1) {db : Pg.PgDb Val} {l : List Memory} (h : PgServed ts tv db l) :
    Pg.Matches valKind ts db ∧ Served (Pg.view ts tv db) l := by
  induction h with
  | fresh hm he => exact ⟨hm, he ▸ .fresh⟩
  | commit _ hl hd hvw hq hr ih =>
    obtain ⟨hm, hS⟩ := ih
    obtain ⟨db2, hr2, hm2, hv2, -, -⟩ := Pg.compileAll_sound valKind hv htv _ _ _ hm hq
    rw [hr] at hr2
    cases hr2
    exact ⟨hm2, hv2 ▸ .commit hS (lists_of hv htv hkl hlen hm hl) hd hvw⟩
  | other _ hne htv' hq hr ih =>
    obtain ⟨hm, hS⟩ := ih
    obtain ⟨db2, hr2, hm2, -, ho, -⟩ := Pg.compileAll_sound valKind hv htv' _ _ _ hm hq
    rw [hr] at hr2
    cases hr2
    exact ⟨hm2, (ho _ (Ne.symm hne)) ▸ hS⟩

/-- `stored` on PostgreSQL: the tenant's rows are the memories its commits
computed, and a load decodes to them, up to row order. -/
theorem pg_stored (hv : Pg.Valid ts) (htv : valKind tv = some .int) (hkl : Pg.klOf ts = kl)
    (hlen : ts.length = 1) {db : Pg.PgDb Val} {l : List Memory} (h : PgServed ts tv db l) :
    app.Holds (Pg.view ts tv db) (enc l) ∧
      ∀ r, Loads ts tv db r → decode r ⦃ o => ∃ snap, o = some snap ∧ app.Equiv snap.memories.val l ⦄ := by
  obtain ⟨hm, hS⟩ := pg_served hv htv hkl hlen h
  exact ⟨(stored hS).1, fun r hr => (stored hS).2 r (lists_of hv htv hkl hlen hm hr)⟩

end calculator_kernel.Storage
