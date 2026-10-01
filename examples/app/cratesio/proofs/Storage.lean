import Apply
/-!
# What the store holds

The server writes with the kernel's `sql_writes` and loads with `decode`.
`H5iAppLib.Store` proves for any schema that the database then holds the
encoding of `applyAll`; this file supplies the registry's encoding and table
writes. Deleting a crate runs as a `SELECT` plus keyed deletes.
-/
open Aeneas Aeneas.Std Result cratesio_kernel cratesio_kernel.Spec cratesio_kernel.Schema H5iAppLib H5iAppLib.Sql H5iAppLib.Store

namespace cratesio_kernel.Storage

/-- A state's rows, table by table. -/
def enc (s : St) : Tables Val
  | 0 => s.users.map User.row
  | 1 => s.sessions.map Session.row
  | 2 => s.tokens.map Token.row
  | 3 => s.crates.map Krate.row
  | 4 => s.versions.map Version.row
  | 5 => s.owners.map Owner.row
  | 6 => s.invites.map Invite.row
  | 7 => s.deps.map Dep.row
  | 8 => [Counter.row s.counter]
  | _ => []

/-- The table writes of one write. -/
def sqlA : Write → List (AWrite Val)
  | .PutUser x => [User.putA x]
  | .PutSession x => [Session.putA x]
  | .PutToken x => [Token.putA x]
  | .PutCrate x => [Krate.putA x]
  | .PutVersion x => [Version.putA x]
  | .PutOwner x => [Owner.putA x]
  | .DelOwner x => [Owner.delA x.krate x.owner x.team]
  | .PutInvite x => [Invite.putA x]
  | .DelInvite k u => [Invite.delA k u]
  | .PutDep x => [Dep.putA x]
  | .DelCrate k => [Krate.delA k, Version.delWhereA 0 (int k.val), Owner.delWhereA 0 (int k.val),
      Invite.delWhereA 0 (int k.val), Dep.delWhereA 0 (int k.val)]
  | .SetCounter c => [Counter.putA c]

set_option maxHeartbeats 2000000 in
theorem enc_step : ∀ s w, applyAllW kl (enc s) (sqlA w) = enc (applyWrite s w) := by
  schema_step [enc, sqlA, applyWrite]

def app : App St Write Val where
  kl := kl
  enc := enc
  sql := sqlA
  step := applyWrite
  init := init
  IsRow := IsRow
  enc_step := enc_step
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
  unfold sql_write; cases w <;> simp only [app, sqlA, List.length_singleton, List.length_cons] at h ⊢ <;>
    step* <;> simp_all [KRATE] <;> omega

/-- The store holds what `apply` computes: every database the server produces
from an empty registry reads back exactly the rows of the state `applyAll`
gives for its commits, and loading it decodes to that state, up to row order. -/
theorem stored {db : Db Val} {s : St} (h : Served app Snapshot.toSt db s) :
    app.Holds db (enc s) ∧
      ∀ r, Lists kl db (Rows.tabs r) → decode r ⦃ o => ∃ snap, o = some snap ∧ app.Equiv (Snapshot.toSt snap) s ⦄ :=
  Schema.stored fits sql_fits h

/-! ## The invariants on PostgreSQL

`Accepted` write sets keep `Inv` (`inv_step`); `Schema.pg_loaded_inv`
carries that through the compiled statements to what a later load decodes.
The counters are `U64`s, so `Inv` needs no bounds. -/

/-- Write sets the kernel returns for a command it accepts. -/
def Accepted (snap : Snapshot) (ws : alloc.vec.Vec Write) : Prop :=
  ∃ p c r, transition p snap c = .ok (.Ok (ws, r))

/-- `Inv` depends on each table only up to row order. -/
theorem inv_perm {s s' : St} (hc : s'.counter = s.counter) (h0 : s'.users.Perm s.users)
    (h1 : s'.sessions.Perm s.sessions) (h2 : s'.tokens.Perm s.tokens) (h3 : s'.crates.Perm s.crates)
    (h4 : s'.versions.Perm s.versions) (h5 : s'.owners.Perm s.owners) (h6 : s'.invites.Perm s.invites)
    (h7 : s'.deps.Perm s.deps) (hi : Inv s) : Inv s' := by
  have hk : ∀ {k}, hasCrate s k = true → hasCrate s' k = true :=
    Invariants.hasCrate_mono fun c hc => ⟨c, h3.symm.subset hc, rfl⟩
  exact {
    user_keys := (h0.map _).nodup_iff.2 hi.user_keys
    session_keys := (h1.map _).nodup_iff.2 hi.session_keys
    token_keys := (h2.map _).nodup_iff.2 hi.token_keys
    crate_keys := (h3.map _).nodup_iff.2 hi.crate_keys
    version_keys := (h4.map _).nodup_iff.2 hi.version_keys
    owner_keys := (h5.map _).nodup_iff.2 hi.owner_keys
    invite_keys := (h6.map _).nodup_iff.2 hi.invite_keys
    dep_keys := (h7.map _).nodup_iff.2 hi.dep_keys
    session_fresh := fun x hx => hc ▸ hi.session_fresh x (h1.subset hx)
    token_fresh := fun t ht => hc ▸ hi.token_fresh t (h2.subset ht)
    owned := fun k hk' => by
      obtain ⟨o, ho, e⟩ := hi.owned k (h3.subset hk')
      exact ⟨o, h5.symm.subset ho, e⟩
    owner_users := fun o ho ht => by
      obtain ⟨u, hu, e⟩ := hi.owner_users o (h5.subset ho) ht
      exact ⟨u, h0.symm.subset hu, e⟩
    version_crates := fun v hv => hk (hi.version_crates v (h4.subset hv))
    owner_crates := fun o ho => hk (hi.owner_crates o (h5.subset ho))
    invite_crates := fun i hi' => hk (hi.invite_crates i (h6.subset hi'))
    dep_crates := fun d hd => hk (hi.dep_crates d (h7.subset hd))
    dep_targets := fun d hd => hk (hi.dep_targets d (h7.subset hd)) }

theorem inv_equiv (snap : Snapshot) (s : St) (h : app.Equiv (Snapshot.toSt snap) s) (hi : Inv s) :
    Inv (Snapshot.toSt snap) := by
  have h0 := h 0
  have h1 := h 1
  have h2 := h 2
  have h3 := h 3
  have h4 := h 4
  have h5 := h 5
  have h6 := h 6
  have h7 := h 7
  have h8 := h 8
  simp only [app, enc, Snapshot.toSt] at h0 h1 h2 h3 h4 h5 h6 h7 h8
  rw [List.map_perm_map_iff User.row_inj] at h0
  rw [List.map_perm_map_iff Session.row_inj] at h1
  rw [List.map_perm_map_iff Token.row_inj] at h2
  rw [List.map_perm_map_iff Krate.row_inj] at h3
  rw [List.map_perm_map_iff Version.row_inj] at h4
  rw [List.map_perm_map_iff Owner.row_inj] at h5
  rw [List.map_perm_map_iff Invite.row_inj] at h6
  rw [List.map_perm_map_iff Dep.row_inj] at h7
  rw [List.perm_singleton, List.cons.injEq] at h8
  exact inv_perm (Counter.row_inj h8.1) h0 h1 h2 h3 h4 h5 h6 h7 hi

/-- The invariants hold on the database. For the schema the server passes to
`h5i_app_pgsql`, every database the server produces, one accepted command at a
time among other commits, holds a state satisfying `Inv`, and every snapshot
a later load decodes satisfies `Inv`. -/
theorem db_inv {ts : List Pg.Tab} {tv : Val} (hv : Pg.Valid ts) (htv : valKind tv = some .int)
    (hkl : Pg.klOf ts = kl) (hlen : ts.length = 9) {db : Pg.PgDb Val} {s : St}
    (h : PgServed app Snapshot.toSt ts tv Accepted db s) :
    Inv s ∧ ∀ r, Loads ts tv db r → decode r ⦃ o => ∃ snap, o = some snap ∧ Inv (Snapshot.toSt snap) ⦄ :=
  pg_loaded_inv sql_fits fits hv htv hkl hlen Inv Invariants.init_inv inv_equiv
    (fun _ _ ⟨_, _, _, ht⟩ hi => Invariants.inv_step hi (Commands.writes_of _ _ _ true _ _ ht)) h

end cratesio_kernel.Storage
