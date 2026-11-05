import H5iAppLib.Authz
import H5iAppLib.ReadOnly
import H5iAppLib.Pg
/-!
# Obligations: the statements h5i fixes

Each `def` here is the statement of one security property, parameterised by
the app's extracted functions. h5i fixes the statement; the framework proves
it once where it can (the guard, the storage section, `tenant_isolation`), and
an app that supplies its own proof must prove exactly this statement for its
own functions: `h5i app check` elaborates the expected type from the
manifest's `[obligations]` slots (and `[authz] storage`) and requires the named
theorem's type to be definitionally equal to it (`docs/design/design-app-authz.md`,
"Obligations").

The arguments `check` fills in come from the extraction and the schema, never
from the app's proofs: the kernel's `transition`, `read_only`, `apply`,
`sql_writes` and `decode`, and the schema's `Snapshot.enc`, `Loads`, `kl`. Where
a statement has a part the app chooses (the write list of a write set that is
not a `Vec`, a hand-written policy's `allowed`), `check` prints what was chosen.
-/
open Aeneas Aeneas.Std Result

namespace H5iAppLib.Obligation

open H5iAppLib.Sql

/-! ## Read-only commands -/

/-- `Kernel::read_only`'s obligation: whenever `readOnly c` is `true` and
`transition` accepts `c`, the write set is empty. The engine runs those
commands in a `READ ONLY DEFERRABLE` transaction on its strength. -/
def ReadOnlySound {P S C WS W R E : Type}
    (readOnly : C → Result Bool)
    (transition : P → S → C → Result (core.result.Result (WS × R) E))
    (writesOf : WS → List W) : Prop :=
  ReadOnly.Sound readOnly transition writesOf

/-! ## Authorization (hand-written policies) -/

/-- A hand-written universal authorization theorem: every write of a
committed command satisfies `allowed`, every disclosed row `visible`, for
every actor, state and command of the kernel's own `transition`. -/
def Authorized {P S C WS W R E St D : Type}
    (transition : P → S → C → Result (core.result.Result (WS × R) E)) (toSt : S → St)
    (writesOf : WS → List W) (allowed : St → P → W → Prop)
    (readsOf : R → List D) (visible : St → P → D → Prop) : Prop :=
  H5iAppLib.Authorized transition toSt writesOf allowed readsOf visible

/-! ## Panic freedom -/

/-- `f` returns for every actor, state and command. Aeneas maps a Rust panic,
an arithmetic overflow and an out-of-bounds index to `fail`, so this says none
of them happens. `⦃ ⦄` is total correctness: it also rules out divergence. -/
def PanicFree {P S C O : Type} (f : P → S → C → Result O) : Prop :=
  ∀ a s c, f a s c ⦃ _ => True ⦄

/-- Panic freedom from a totality theorem (`∃ r, f a s c = ok r`). -/
theorem panicFree_of_total {P S C O : Type} {f : P → S → C → Result O}
    (h : ∀ a s c, ∃ r, f a s c = ok r) : PanicFree f := by
  intro a s c
  obtain ⟨r, hr⟩ := h a s c
  rw [hr]
  exact WP.spec.ret trivial

/-! ## Tenant isolation -/

/-- Cross-tenant noninterference on the database model `H5iAppLib.Pg`, for
every schema `h5i_app_pgsql::valid` accepts:

- statements compiled for another tenant `tv'` leave tenant `tv`'s rows
  (`Pg.view ts tv`) as they were;
- what a compiled `SELECT` for `tv` returns depends only on `tv`'s rows: two
  databases that agree on them give the same rows, up to order.

So no other tenant's commits change what a tenant's commands load, and no
other tenant's rows reach them. `kind` is the schema's `valKind`. -/
def TenantIsolation {V : Type} (kind : V → Option Pg.Kind) : Prop :=
  (∀ (ts : List Pg.Tab) (tv tv' : V) (ss : List (AStmt V)) (qs : List (Pg.Sql × List V))
      (db db' : Pg.PgDb V),
    Pg.Valid ts → kind tv' = some .int → tv' ≠ tv → Pg.Matches kind ts db →
    ss.mapM (Pg.compileA kind ts tv') = some qs → Pg.runAll kind db qs = some db' →
    Pg.view ts tv db' = Pg.view ts tv db) ∧
  (∀ (ts : List Pg.Tab) (tv : V) (db₁ db₂ : Pg.PgDb V) (t : Nat) (f : Option (Nat × V))
      (q : Pg.Sql) (ps : List V) (R₁ R₂ : List (List V)),
    Pg.Valid ts → kind tv = some .int → Pg.Matches kind ts db₁ → Pg.Matches kind ts db₂ →
    Pg.view ts tv db₁ = Pg.view ts tv db₂ → Pg.selectA kind ts tv t f = some (q, ps) →
    Pg.selected kind db₁ q ps = some R₁ → Pg.selected kind db₂ q ps = some R₂ → R₁.Perm R₂)

/-- Proven once, from `Pg.compileAll_sound` and `Pg.select_sound`. -/
theorem tenant_isolation {V : Type} (kind : V → Option Pg.Kind) : TenantIsolation kind := by
  refine ⟨?_, ?_⟩
  · intro ts tv tv' ss qs db db' hv htv' hne hm hq hr
    obtain ⟨db2, hr2, -, -, ho, -⟩ := Pg.compileAll_sound kind hv htv' ss qs db hm hq
    rw [hr] at hr2
    cases hr2
    exact ho tv (Ne.symm hne)
  · intro ts tv db₁ db₂ t f q ps R₁ R₂ hv htv hm₁ hm₂ heq hc h₁ h₂
    obtain ⟨R₁', e₁, hs₁⟩ := Pg.select_sound kind hv htv hm₁ hc
    obtain ⟨R₂', e₂, hs₂⟩ := Pg.select_sound kind hv htv hm₂ hc
    rw [h₁] at e₁
    rw [h₂] at e₂
    cases e₁
    cases e₂
    have a := hs₁ R₁ (.refl _)
    have b := hs₂ R₂ (.refl _)
    rw [heq] at a
    exact (List.perm_ext_iff_of_nodup a.1 b.1).2 (fun row => (a.2 row).trans (b.2 row).symm)

/-! ## Storage (A4) -/

/-- One tenant's databases as the engine produces them, with the snapshot
each holds, computed by the kernel's own functions: a commit loads the rows
(`loads`), `decode`s them, runs the kernel's `apply` on an accepted write set
(`Step`, with the size bound `bound`), and stores `sqlWrites` compiled for
the tenant; other tenants' compiled statements may run in between. The
statement starts from a fresh tenant, whose snapshot is `init`. -/
inductive Reached {V Rw S W SW : Type} (kind : V → Option Pg.Kind) (ts : List Pg.Tab) (tv : V)
    (loads : Pg.PgDb V → Rw → Prop) (decode : Rw → Result (Option S))
    (sqlWrites : alloc.vec.Vec W → Result (alloc.vec.Vec SW)) (sqlW : SW → AWrite V)
    (apply : S → alloc.vec.Vec W → Result S) (init : S)
    (Step : S → alloc.vec.Vec W → Prop) (bound : S → alloc.vec.Vec W → Prop) :
    Pg.PgDb V → S → Prop
  | fresh {db : Pg.PgDb V} :
      Pg.Matches kind ts db → Pg.view ts tv db = (fun _ _ => none) →
      Reached kind ts tv loads decode sqlWrites sqlW apply init Step bound db init
  | commit {db db' : Pg.PgDb V} {s snap s' : S} {r : Rw} {ws : alloc.vec.Vec W}
      {v : alloc.vec.Vec SW} {qs : List (Pg.Sql × List V)} :
      Reached kind ts tv loads decode sqlWrites sqlW apply init Step bound db s →
      loads db r → decode r = ok (some snap) → Step snap ws → bound snap ws →
      sqlWrites ws = ok v → ((v.val.map sqlW).map planA).mapM (Pg.compileA kind ts tv) = some qs →
      Pg.runAll kind db qs = some db' → apply snap ws = ok s' →
      Reached kind ts tv loads decode sqlWrites sqlW apply init Step bound db' s'
  | other {db db' : Pg.PgDb V} {s : S} {tv' : V} {ss : List (AStmt V)} {qs : List (Pg.Sql × List V)} :
      Reached kind ts tv loads decode sqlWrites sqlW apply init Step bound db s →
      tv' ≠ tv → kind tv' = some .int → ss.mapM (Pg.compileA kind ts tv') = some qs →
      Pg.runAll kind db qs = some db' →
      Reached kind ts tv loads decode sqlWrites sqlW apply init Step bound db' s

/-- A4 for an app: on PostgreSQL (as `H5iAppLib.Pg` models it), for every
schema description `ts` that is valid and numbered like the schema (`kl`, `n`
tables), every tenant and every sequence of accepted write sets: every load
of the tenant's rows decodes to a snapshot whose tables hold the rows of the
state the kernel's `apply` computed, up to row order (`enc`). -/
def Storage {V Rw S W SW : Type} (kind : V → Option Pg.Kind)
    (loads : List Pg.Tab → V → Pg.PgDb V → Rw → Prop) (decode : Rw → Result (Option S))
    (sqlWrites : alloc.vec.Vec W → Result (alloc.vec.Vec SW)) (sqlW : SW → AWrite V)
    (apply : S → alloc.vec.Vec W → Result S) (init : S) (enc : S → Tables V)
    (bound : S → alloc.vec.Vec W → Prop) (kl : Nat → Nat) (n : Nat) : Prop :=
  ∀ (ts : List Pg.Tab) (tv : V) (Step : S → alloc.vec.Vec W → Prop) (db : Pg.PgDb V) (s : S),
    Pg.Valid ts → kind tv = some .int → Pg.klOf ts = kl → ts.length = n →
    Reached kind ts tv (loads ts tv) decode sqlWrites sqlW apply init Step bound db s →
    ∀ r, loads ts tv db r → decode r ⦃ o => ∃ snap, o = some snap ∧ ∀ t, (enc snap t).Perm (enc s t) ⦄

end H5iAppLib.Obligation
