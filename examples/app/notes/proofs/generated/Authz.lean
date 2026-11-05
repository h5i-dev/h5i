-- Written by `h5i app extract` from `[authz] guarded` in h5i-app.toml. Do not edit.
import AuthzSpecs
import Schema
import H5iAppLib.Obligation
/-!
# Framework-proven authorization

Each `h5i_authz g` reads the policy's instances off `g`'s extracted body and
proves `g.authorized` by instantiating `h5i_app_std.Specs.authz`: every
committed command makes only writes the policy permits and discloses only rows
it lets the caller see. For `guard_viewed` it also proves noninterference of
read-only commands and `H5iAppLib.ReadOnly.Sound` (`g.read_only_sound`).
-/
open Aeneas Aeneas.Std Result H5iAppLib notes_kernel

h5i_derive_all

h5i_authz notes_kernel.guarded_transition

/-! ## The store commits what `apply` computes

A4 for this app, with no hand-written model: the state is the table encoding
itself (`trunc` keeps the schema's 2 tables), so `enc_step` holds by
construction. What is checked against the extracted code is that `sql_write`
emits `sqlA` (`sql_fits`) and that `apply_write` changes `Snapshot.enc`
exactly as `sqlA` does (`apply_write_enc`). Then `apply_enc`: committing a
write set leaves the encoding of the kernel's `apply`; `pg_stored`: on
PostgreSQL, the tenant's tables hold that state and every load decodes to it,
up to row order. `sqlA` is read off the extracted `sql_write`; a wrong reading
fails here. -/

set_option linter.unusedSimpArgs false

namespace notes_kernel.Committed
open notes_kernel.Schema H5iAppLib.Sql H5iAppLib.Store

def sqlA : Write → List (AWrite Val)
  | .PutMember m => [Member.putA m]
  | .DelMember user => [Member.delA user]
  | .PutNote n => [Note.putA n]
  | .DelNote id => [Note.delA id]

def trunc (T : Tables Val) : Tables Val := fun t => if t < 2 then T t else []

theorem targets (w : Write) : ∀ a ∈ sqlA w, target a < 2 := by
  cases w <;> simp [sqlA, target, Member.putA, Member.table, Member.delA, Member.delWhereA, Note.putA, Note.table, Note.delA, Note.delWhereA]

theorem enc_nil (s : Snapshot) (t : Nat) (h : 2 ≤ t) : Snapshot.enc s t = [] := by
  obtain ⟨k, rfl⟩ := Nat.exists_eq_add_of_le' h
  simp only [Snapshot.enc]

theorem trunc_enc (s : Snapshot) : trunc (Snapshot.enc s) = Snapshot.enc s := by
  funext t; by_cases ht : t < 2
  · simp [trunc, ht]
  · simp [trunc, ht, enc_nil s t (by omega)]

theorem enc_step (T : Tables Val) (w : Write) :
    applyAllW kl (trunc T) (sqlA w) = trunc (applyAllW kl T (sqlA w)) := by
  funext t
  by_cases ht : t < 2
  · simp only [trunc, ht, if_true]
    exact applyAllW_at kl _ _ _ t (by simp [trunc, ht])
  · rw [applyAllW_other kl _ t (fun a ha => by have := targets w a ha; omega)]
    simp [trunc, ht]

noncomputable def app : App (Tables Val) Write Val where
  kl := kl
  enc := trunc
  sql := sqlA
  step T w := applyAllW kl T (sqlA w)
  init := Snapshot.enc Snapshot.empty
  IsRow := IsRow
  enc_step := enc_step
  sql_ok := by schema_ok [sqlA]
  init_ok := by rw [trunc_enc]; schema_init [Snapshot.enc, Snapshot.empty]
  init_rows := by rw [trunc_enc]; schema_rows [Snapshot.enc, Snapshot.empty]

theorem fits : Fits app Snapshot.enc where
  kl := rfl
  rows := rfl
  enc s := trunc_enc s
  init := trunc_enc _
  nil s t h := by simp [app, trunc]; omega

/-- `sql_write` emits `sqlA`. -/
theorem sql_fits : SqlFits app := by
  intro w out h
  unfold sql_write; cases w <;> simp only [app, sqlA, List.length_singleton, List.length_nil] at h ⊢ <;> step* <;> simp_all

/-- `apply_write` changes the encoding exactly as `sqlA` does. -/
theorem apply_write_enc (s : Snapshot) (w : Write) (h : Snapshot.size s < Usize.max) :
    apply_write s w ⦃ s' => Snapshot.enc s' = app.step (Snapshot.enc s) w ∧
      Snapshot.size s' ≤ Snapshot.size s + 1 ⦄ := by
  unfold Snapshot.size at *
  unfold apply_write
  cases w <;> step*
  all_goals (try refine ⟨?_, ?_⟩)
  all_goals
    first
    | (funext t; cases_table t <;> simp_all [app, sqlA, Snapshot.enc, applyAllW, applyW, kl, upsert, Member.putA, Member.table, Member.delA, Member.delWhereA, Note.putA, Note.table, Note.delA, Note.delWhereA, Member.keyLen, ← Member.map_put, ← Member.map_del, ← Member.map_del_where, Note.keyLen, ← Note.map_put, ← Note.map_del, ← Note.map_del_where] <;>
        (congr 1; funext r; congr 1; exact decide_eq_decide.2 Iff.rfl))
    | (simp_all [alloc.vec.Vec.length] <;> grind [upsert_length, List.length_filter_le])

/-- Committing a write set leaves the encoding of the kernel's `apply`. -/
theorem apply_enc (s : Snapshot) (ws : alloc.vec.Vec Write) (h : Snapshot.size s + ws.length < Usize.max) :
    apply s ws ⦃ s' => Snapshot.enc s' = ws.val.foldl app.step (Snapshot.enc s) ⦄ :=
  apply_spec' Snapshot.enc app.step apply_write_enc (fun w => by simp) s ws h

/-- On PostgreSQL, for any accepted write sets (`Step`) interleaved with other
tenants' statements: the tenant's tables hold the state, and every load
decodes to it up to row order. -/
theorem pg_stored {ts : List Pg.Tab} {tv : Val} {Step : Snapshot → alloc.vec.Vec Write → Prop}
    (hv : Pg.Valid ts) (htv : valKind tv = some .int) (hkl : Pg.klOf ts = kl) (hlen : ts.length = 2)
    {db : Pg.PgDb Val} {s : Tables Val} (h : PgServed app Snapshot.enc ts tv Step db s) :
    app.Holds (Pg.view ts tv db) (app.enc s) ∧
      ∀ r, Loads ts tv db r → decode r ⦃ o => ∃ snap, o = some snap ∧ app.Equiv (Snapshot.enc snap) s ⦄ := by
  obtain ⟨hm, hS⟩ := pg_served sql_fits fits hv htv hkl hlen h
  refine ⟨(stored fits sql_fits hS).1, fun r hr => ?_⟩
  have hl : Lists kl (Pg.view ts tv db) (Rows.tabs r) := by
    rw [← hkl]
    exact Pg.lists_of_selects valKind hv htv hm _ (fun t ht => Rows.tabs_nil _ t (hlen ▸ ht))
      (fun t ht => hr t (hlen ▸ ht))
  exact (stored fits sql_fits hS).2 r hl

/-! ### The framework's storage obligation

`H5iAppLib.Obligation.Storage` for this kernel: A4 as h5i states it, over the
kernel's own `apply`, `sql_writes` and `decode`, with no reference to `app`.
`h5i app check` requires `storage` to have exactly that statement. -/

theorem sqlA_length (w : Write) : (sqlA w).length ≤ 1 := by
  cases w <;> simp [sqlA]

theorem flat_length (ws : List Write) : (ws.flatMap sqlA).length ≤ ws.length := by
  induction ws with
  | nil => simp
  | cons w ws ih =>
    simp only [List.flatMap_cons, List.length_append, List.length_cons]
    have := sqlA_length w
    omega

/-- The databases the obligation quantifies over are `PgServed` ones, holding
the encoding of the snapshot `apply` computed. -/
theorem reached_served {ts : List Pg.Tab} {tv : Val} {Step : Snapshot → alloc.vec.Vec Write → Prop}
    {db : Pg.PgDb Val} {s : Snapshot}
    (h : H5iAppLib.Obligation.Reached valKind ts tv (Loads ts tv) decode sql_writes sqlW apply Snapshot.empty Step
      (fun snap ws => Snapshot.size snap + ws.length < Usize.max) db s) :
    PgServed app Snapshot.enc ts tv Step db (Snapshot.enc s) := by
  induction h with
  | fresh hm he => exact .fresh hm he
  | @commit db db' s snap s' r ws v qs _ hl hd hst hb hv hq hr ha ih =>
    have e : Snapshot.enc s' = ws.val.foldl app.step (Snapshot.enc snap) := by
      have := post_of_ok (apply_enc snap ws hb) ha
      exact this
    rw [e]
    have hf : (ws.val.flatMap app.sql).length ≤ Usize.max := by
      have h1 := flat_length ws.val
      have h2 : ws.val.length < Usize.max := by simp only [alloc.vec.Vec.length] at hb; omega
      show (ws.val.flatMap sqlA).length ≤ Usize.max
      omega
    exact .commit ih hl hd hst hf hv hq hr
  | other _ hne htv' hq hr ih => exact .other ih hne htv' hq hr

/-- A4, as `H5iAppLib.Obligation.Storage` states it, for this kernel. -/
theorem storage : H5iAppLib.Obligation.Storage valKind (fun ts tv db r => Loads ts tv db r) decode sql_writes sqlW
    apply Snapshot.empty Snapshot.enc (fun snap ws => Snapshot.size snap + ws.length < Usize.max) kl 2 := by
  intro ts tv Step db s hv htv hkl hlen h r hr
  apply WP.spec_mono ((pg_stored hv htv hkl hlen (reached_served h)).2 r hr)
  rintro o ⟨snap, rfl, he⟩
  refine ⟨snap, rfl, fun t => ?_⟩
  have := he t
  simp only [app] at this
  rwa [trunc_enc, trunc_enc] at this

end notes_kernel.Committed
