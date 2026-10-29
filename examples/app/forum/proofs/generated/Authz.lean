-- Written by `h5i app extract` from `[authz] guarded` in h5i-app.toml. Do not edit.
import AuthzSpecs
import Schema
/-!
# Framework-proven authorization

Each `h5i_authz g` reads the policy's instances off `g`'s extracted body and
proves `g.authorized` by instantiating `h5i_app_std.Specs.authz`: every
committed command makes only writes the policy permits and discloses only rows
it lets the caller see. For `guard_viewed` it also proves noninterference of
read-only commands and `H5iAppLib.ReadOnly.Sound` (`g.read_only_sound`).
-/
open Aeneas Aeneas.Std Result H5iAppLib forum_kernel

h5i_derive_all

h5i_authz forum_kernel.guarded_transition

/-! ## The store commits what `apply` computes

A4 for this app, with no hand-written model: the state is the table encoding
itself (`trunc` keeps the schema's 3 tables), so `enc_step` holds by
construction. What is checked against the extracted code is that `sql_write`
emits `sqlA` (`sql_fits`) and that `apply_write` changes `Snapshot.enc`
exactly as `sqlA` does (`apply_write_enc`). Then `apply_enc`: committing a
write set leaves the encoding of the kernel's `apply`; `pg_stored`: on
PostgreSQL, the tenant's tables hold that state and every load decodes to it,
up to row order. `sqlA` is read off the extracted `sql_write`; a wrong reading
fails here. -/

set_option linter.unusedSimpArgs false

namespace forum_kernel.Committed
open forum_kernel.Schema H5iAppLib.Sql H5iAppLib.Store

def sqlA : Write → List (AWrite Val)
  | .PutUser u => [User.putA u]
  | .PutPost p => [Post.putA p]
  | .DelPost id => [Post.delA id]
  | .SetCounter c => [Counter.putA c]

def trunc (T : Tables Val) : Tables Val := fun t => if t < 3 then T t else []

theorem targets (w : Write) : ∀ a ∈ sqlA w, target a < 3 := by
  cases w <;> simp [sqlA, target, User.putA, User.table, User.delA, User.delWhereA, Post.putA, Post.table, Post.delA, Post.delWhereA, Counter.putA, Counter.table]

theorem enc_nil (s : Snapshot) (t : Nat) (h : 3 ≤ t) : Snapshot.enc s t = [] := by
  obtain ⟨k, rfl⟩ := Nat.exists_eq_add_of_le' h
  simp only [Snapshot.enc]

theorem trunc_enc (s : Snapshot) : trunc (Snapshot.enc s) = Snapshot.enc s := by
  funext t; by_cases ht : t < 3
  · simp [trunc, ht]
  · simp [trunc, ht, enc_nil s t (by omega)]

theorem enc_step (T : Tables Val) (w : Write) :
    applyAllW kl (trunc T) (sqlA w) = trunc (applyAllW kl T (sqlA w)) := by
  funext t
  by_cases ht : t < 3
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
    | (funext t; cases_table t <;> simp_all [app, sqlA, Snapshot.enc, applyAllW, applyW, kl, upsert, User.putA, User.table, User.delA, User.delWhereA, Post.putA, Post.table, Post.delA, Post.delWhereA, Counter.putA, Counter.table, User.keyLen, ← User.map_put, ← User.map_del, ← User.map_del_where, Post.keyLen, ← Post.map_put, ← Post.map_del, ← Post.map_del_where, Counter.keyLen, Counter.row] <;>
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
    (hv : Pg.Valid ts) (htv : valKind tv = some .int) (hkl : Pg.klOf ts = kl) (hlen : ts.length = 3)
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

end forum_kernel.Committed
