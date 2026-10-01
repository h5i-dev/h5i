import Apply
/-!
# What the store holds

The server stores a write set by running the plan of `sql_writes`, and loads
rows through `decode`. `H5iAppLib.Store` proves, for any schema, that the database
then holds the encoding of the state `applyAll` computes. This file gives
the inbox's encoding and table writes.
-/
open Aeneas Aeneas.Std Result inbox_kernel inbox_kernel.Spec inbox_kernel.Schema H5iAppLib H5iAppLib.Sql H5iAppLib.Store

namespace inbox_kernel.Storage

/-- A state's rows, table by table. -/
def enc (s : St) : Tables Val
  | 0 => s.msgs.map Message.row
  | 1 => s.blocks.map Block.row
  | _ => []

/-- The table writes of one write. -/
def sqlA : Write → List (AWrite Val)
  | .PutMessage m => [Message.putA m]
  | .PutBlock b => [Block.putA b]
  | .DelBlock b => [Block.delA b.owner b.sender]

theorem msgKey_eq : msgKey = fun m : Message => (m.sender, m.recipient, m.seq) := rfl
theorem blockKey_eq : blockKey = fun b : Block => (b.owner, b.sender) := rfl

def app : App St Write Val where
  kl := kl
  enc := enc
  sql := sqlA
  step := applyWrite
  init := init
  IsRow := IsRow
  enc_step := by schema_step [enc, sqlA, applyWrite, msgKey_eq, blockKey_eq]
  sql_ok := by schema_ok [sqlA]
  init_ok := by schema_init [enc, init]
  init_rows := by schema_rows [enc, init]

theorem fits : Fits app Snapshot.toSt where
  kl := rfl
  rows := rfl
  enc s := by funext t; cases_table t <;> rfl
  init := by funext t; cases_table t <;> rfl
  nil s t h := by cases_table t <;> first | omega | rfl

theorem sql_fits : SqlFits app := by
  intro w out h
  unfold sql_write; cases w <;> simp only [app, sqlA, List.length_singleton] at h ⊢ <;> step* <;> simp_all

/-- The store holds what `apply` computes: the rows of the state `applyAll`
gives for its commits, and a load decodes to that state, up to row order. -/
theorem stored {db : Db Val} {s : St} (h : Served app Snapshot.toSt db s) :
    app.Holds db (enc s) ∧
      ∀ r, Lists kl db (Rows.tabs r) → decode r ⦃ o => ∃ snap, o = some snap ∧ app.Equiv (Snapshot.toSt snap) s ⦄ :=
  Schema.stored fits sql_fits h

/-! ## The invariants on PostgreSQL

From a command to a later load: `transition` accepts a write set
(`Accepted`), `inv_preserved` keeps `Inv`, the server runs the compiled
`sql_writes`, and a load runs the compiled `SELECT`s and `decode`.
`Schema.pg_loaded_inv` chains them. -/

/-- Write sets the kernel returns for a command it accepts. -/
def Accepted (snap : Snapshot) (ws : alloc.vec.Vec Write) : Prop :=
  ∃ a c r, transition a snap c = .ok (.Ok (ws, r))

/-- `Inv` survives reordering rows. The state has no counters, so no bound
is needed. -/
theorem inv_equiv (snap : Snapshot) (s : St) (h : app.Equiv (Snapshot.toSt snap) s) (hi : Inv s) :
    Inv (Snapshot.toSt snap) := by
  have h0 := h 0
  have h1 := h 1
  simp only [app, enc, Snapshot.toSt] at h0 h1
  rw [List.map_perm_map_iff Message.row_inj] at h0
  rw [List.map_perm_map_iff Block.row_inj] at h1
  obtain ⟨hk, ht, hb⟩ := hi
  exact ⟨(h0.map _).nodup_iff.2 hk, fun m hm => ht m (h0.subset hm), (h1.map _).nodup_iff.2 hb⟩

/-- The invariants hold on the database: for the server's schema and any
tenant, accepted commands interleaved with other tenants' commits leave a
state satisfying `Inv`, and so does every snapshot a later load decodes. -/
theorem db_inv {ts : List Pg.Tab} {tv : Val} (hv : Pg.Valid ts) (htv : valKind tv = some .int)
    (hkl : Pg.klOf ts = kl) (hlen : ts.length = 2) {db : Pg.PgDb Val} {s : St}
    (h : PgServed app Snapshot.toSt ts tv Accepted db s) :
    Inv s ∧ ∀ r, Loads ts tv db r → decode r ⦃ o => ∃ snap, o = some snap ∧ Inv (Snapshot.toSt snap) ⦄ :=
  pg_loaded_inv sql_fits fits hv htv hkl hlen Inv Theorems.init_inv inv_equiv
    (fun snap ws ⟨a, c, r, ht⟩ hi => Theorems.inv_preserved a snap c ws r hi ht) h

end inbox_kernel.Storage
