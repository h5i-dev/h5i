import Apply
/-!
# What the store holds

The server stores a write set by running the plan of `sql_writes`, and loads
rows through `decode`. `H5iAppLib.Store` proves, for any schema, that the database
then holds the encoding of the state `applyAll` computes. This file gives
the encoding and table writes; effects go to the outbox, not a table.
-/
open Aeneas Aeneas.Std Result booking_kernel booking_kernel.Spec booking_kernel.Schema H5iAppLib H5iAppLib.Sql H5iAppLib.Store

namespace booking_kernel.Storage

/-- A state's rows, table by table. -/
def enc (s : St) : Tables Val
  | 0 => s.admins.map Admin.row
  | 1 => s.rooms.map Room.row
  | 2 => s.bookings.map Booking.row
  | 3 => [[int s.next]]
  | _ => []

/-- The table writes of one write. -/
def sqlA : Write → List (AWrite Val)
  | .PutAdmin x => [Admin.putA x]
  | .PutRoom x => [Room.putA x]
  | .PutBooking x => [Booking.putA x]
  | .DelBooking id => [Booking.delA id]
  | .SetCounter c => [Counter.putA c]
  | .Emit _ => []

def app : App St Write Val where
  kl := kl
  enc := enc
  sql := sqlA
  step := applyWrite
  init := init
  IsRow := IsRow
  enc_step := by schema_step [enc, sqlA, applyWrite]
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

/-- Write sets the kernel returns for a command it accepts, at any time. -/
def Accepted (snap : Snapshot) (ws : alloc.vec.Vec Write) : Prop :=
  ∃ a c r, transition a snap c = .ok (.Ok (ws, r))

/-- The invariants, and the counter fits its `BIGINT`. -/
def DbInv (s : St) : Prop := Inv s ∧ s.next < 2 ^ 64

theorem next_bound (s : St) (ws : List Write) (h : s.next < 2 ^ 64) : (applyAll s ws).next < 2 ^ 64 := by
  induction ws generalizing s with
  | nil => exact h
  | cons w ws ih =>
    apply ih
    cases w <;> simp only [applyWrite] <;> first | exact h | scalar_tac

theorem dbInv_equiv (snap : Snapshot) (s : St) (h : app.Equiv (Snapshot.toSt snap) s) (hi : DbInv s) :
    DbInv (Snapshot.toSt snap) := by
  have h0 := h 0
  have h1 := h 1
  have h2 := h 2
  have h3 := h 3
  simp only [app, enc, Snapshot.toSt] at h0 h1 h2 h3
  rw [List.map_perm_map_iff Admin.row_inj] at h0
  rw [List.map_perm_map_iff Room.row_inj] at h1
  rw [List.map_perm_map_iff Booking.row_inj] at h2
  rw [List.perm_singleton, List.cons.injEq] at h3
  have hn : snap.counter.next_id.val = s.next := int_inj (by scalar_tac) hi.2 (List.cons.inj h3.1).1
  obtain ⟨⟨hrk, hbk, hrf, hbf, hbr, hne, hc⟩, hb⟩ := hi
  refine ⟨⟨?_, ?_, fun r hr => ?_, fun b hb => ?_, fun b hb => ?_, fun b hb => hne b (h2.subset hb), ?_⟩,
    by simp only [Snapshot.toSt]; scalar_tac⟩
  · exact (h1.map _).nodup_iff.2 hrk
  · exact (h2.map _).nodup_iff.2 hbk
  · simp only [Snapshot.toSt, hn]; exact hrf r (h1.subset hr)
  · simp only [Snapshot.toSt, hn]; exact hbf b (h2.subset hb)
  · obtain ⟨r, hr, e⟩ := hbr b (h2.subset hb)
    exact ⟨r, h1.symm.subset hr, e⟩
  · exact (h2.pairwise_iff fun h => Theorems.compatible_comm h).2 hc

/-- The invariants hold on the database: for the server's schema and any
tenant, accepted commands interleaved with other tenants' commits leave a
state satisfying `Inv`, and so does every snapshot a later load decodes. -/
theorem db_inv {ts : List Pg.Tab} {tv : Val} (hv : Pg.Valid ts) (htv : valKind tv = some .int)
    (hkl : Pg.klOf ts = kl) (hlen : ts.length = 4) {db : Pg.PgDb Val} {s : St}
    (h : PgServed app Snapshot.toSt ts tv Accepted db s) :
    Inv s ∧ ∀ r, Loads ts tv db r → decode r ⦃ o => ∃ snap, o = some snap ∧ Inv (Snapshot.toSt snap) ⦄ := by
  obtain ⟨hi, hload⟩ := pg_loaded_inv sql_fits fits hv htv hkl hlen DbInv ⟨Theorems.init_inv, by simp [app, init]⟩
    dbInv_equiv (fun snap ws ⟨a, c, r, ht⟩ hi => ⟨Theorems.inv_preserved a snap c ws r hi.1 ht,
      next_bound _ _ (by simp only [Snapshot.toSt]; scalar_tac)⟩) h
  refine ⟨hi.1, fun r hr => WP.spec_mono (hload r hr) ?_⟩
  rintro o ⟨snap, rfl, hs⟩
  exact ⟨snap, rfl, hs.1⟩

end booking_kernel.Storage
