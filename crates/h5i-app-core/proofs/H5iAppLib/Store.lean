import H5iAppLib.Sql
/-!
# The store holds what `apply` computes

Proven once for every app. An app gives its state encoding (`enc`), the table
writes of each kernel write (`sql`) and the state step (`step`), and proves
`enc_step`. `App.served_holds`: after any commits, the database reads back the
encoding of the state `applyAll` computes. `App.served_lists`: a `SELECT` of
every table returns that encoding up to row order. `H5iAppLib.Pg` links both to SQL.
-/

namespace H5iAppLib.Store

open Classical H5iAppLib.Sql

variable {V : Type}

section Tabs
variable (kl : Nat → Nat)

/-- A fresh tenant's tables: empty, except that a table without key columns
starts with one row (the snapshot's default counter). -/
def InitOk (I : Tables V) : Prop := ∀ t, I t = [] ∨ (kl t = 0 ∧ (I t).length = 1)

/-- The database holds the tables `E`, where `I` are the fresh tenant's: it
reads back tables equal to `E`, except that a table may still be empty while
`E` has its fresh rows. -/
def Holds (I : Tables V) (db : Db V) (E : Tables V) : Prop :=
  ∃ tabs, db = readBack kl tabs ∧ WellKeyed kl tabs ∧ ∀ t, tabs t = E t ∨ (tabs t = [] ∧ E t = I t)

/-- The table a write changes. -/
def target : AWrite V → Nat
  | .put t _ _ => t
  | .del t _ => t
  | .delWhere t _ _ => t
end Tabs

/-! ## Writes act table by table -/

theorem applyW_at (kl : Nat → Nat) (tabs tabs' : Tables V) (a : AWrite V) (t : Nat) (h : tabs t = tabs' t) :
    applyW kl tabs a t = applyW kl tabs' a t := by
  cases a <;> simp only [applyW] <;> split <;> simp_all

theorem applyAllW_at (kl : Nat → Nat) (ws : List (AWrite V)) :
    ∀ (tabs tabs' : Tables V) (t : Nat), tabs t = tabs' t → applyAllW kl tabs ws t = applyAllW kl tabs' ws t := by
  induction ws with
  | nil => intro _ _ _ h; exact h
  | cons a ws ih =>
    intro tabs tabs' t h
    exact ih _ _ t (applyW_at kl tabs tabs' a t h)

theorem applyW_other (kl : Nat → Nat) (tabs : Tables V) (a : AWrite V) (t : Nat) (h : target a ≠ t) :
    applyW kl tabs a t = tabs t := by
  cases a <;> simp only [applyW, target] at h ⊢ <;> simp [Ne.symm h]

theorem applyAllW_other (kl : Nat → Nat) (ws : List (AWrite V)) (t : Nat) (h : ∀ a ∈ ws, target a ≠ t) :
    ∀ tabs : Tables V, applyAllW kl tabs ws t = tabs t := by
  induction ws with
  | nil => intro _; rfl
  | cons a ws ih =>
    intro tabs
    simp only [applyAllW, List.foldl_cons] at ih ⊢
    rw [ih (fun b hb => h b (by simp [hb])), applyW_other kl tabs a t (h a (by simp))]

/-- On a table without key columns, a put leaves exactly its row, whatever
single row (or none) was there. -/
theorem upsert_single (row : List V) (l : List (List V)) (h : l.length ≤ 1) :
    upsert (·.take 0) row l = [row] := by
  match l, h with
  | [], _ => rfl
  | [x], _ => simp [upsert]

/-- A table without key columns ends in the same state from any start with
at most one row, once some write touches it. -/
theorem applyAllW_single (kl : Nat → Nat) (t : Nat) (hkl : kl t = 0) (ws : List (AWrite V))
    (hok : ∀ a ∈ ws, WriteOk kl a) (ht : ∃ a ∈ ws, target a = t) :
    ∀ tabs tabs' : Tables V, (tabs t).length ≤ 1 → (tabs' t).length ≤ 1 →
      applyAllW kl tabs ws t = applyAllW kl tabs' ws t := by
  induction ws with
  | nil => obtain ⟨a, ha, _⟩ := ht; cases ha
  | cons a ws ih =>
    intro tabs tabs' h1 h2
    simp only [applyAllW, List.foldl_cons]
    by_cases hat : target a = t
    · apply applyAllW_at
      have hw := hok a (by simp)
      cases a with
      | put t0 n row =>
        simp only [target] at hat; subst hat
        simp only [applyW, if_true, hkl]
        rw [upsert_single row _ h1, upsert_single row _ h2]
      | del t0 k => simp only [target] at hat; subst hat; simp [WriteOk, hkl] at hw
      | delWhere t0 i v => simp only [target] at hat; subst hat; simp [WriteOk, hkl] at hw
    · have ht' : ∃ b ∈ ws, target b = t := by
        obtain ⟨b, hb, hbt⟩ := ht
        rcases List.mem_cons.1 hb with rfl | hb
        · exact absurd hbt hat
        · exact ⟨b, hb, hbt⟩
      exact ih (fun b hb => hok b (by simp [hb])) ht' _ _
        (by rw [applyW_other kl tabs a t hat]; exact h1) (by rw [applyW_other kl tabs' a t hat]; exact h2)

/-! ## Keyed lists -/

theorem find_key_iff {α κ : Type} [DecidableEq κ] (key : α → κ) (l : List α) (hn : (l.map key).Nodup)
    (k : κ) (r : α) : l.find? (fun x => key x = k) = some r ↔ r ∈ l ∧ key r = k := by
  constructor
  · intro h
    exact ⟨List.mem_of_find?_eq_some h, by simpa using List.find?_some h⟩
  · rintro ⟨hr, rfl⟩
    obtain ⟨r', hr'⟩ : ∃ r', l.find? (fun x => key x = key r) = some r' := by
      cases h : l.find? (fun x => key x = key r) with
      | some r' => exact ⟨r', rfl⟩
      | none => simp [List.find?_eq_none] at h; exact absurd rfl (h r hr)
    have hm := List.mem_of_find?_eq_some hr'
    have hk : key r' = key r := by simpa using List.find?_some hr'
    rw [hr', List.inj_on_of_nodup_map hn hm hr hk]

/-- Two orders of the same keyed rows read back the same. -/
theorem find_perm {α κ : Type} [DecidableEq κ] (key : α → κ) {l l' : List α} (hp : l.Perm l')
    (hn : (l.map key).Nodup) (k : κ) :
    l.find? (fun x => key x = k) = l'.find? (fun x => key x = k) := by
  have hn' : (l'.map key).Nodup := (hp.map key).nodup_iff.1 hn
  cases h : l.find? (fun x => key x = k) with
  | some r =>
    obtain ⟨hr, hk⟩ := (find_key_iff key l hn k r).1 h
    exact ((find_key_iff key l' hn' k r).2 ⟨hp.subset hr, hk⟩).symm
  | none =>
    cases h' : l'.find? (fun x => key x = k) with
    | none => rfl
    | some r =>
      obtain ⟨hr, hk⟩ := (find_key_iff key l' hn' k r).1 h'
      rw [(find_key_iff key l hn k r).2 ⟨hp.symm.subset hr, hk⟩] at h
      cases h

/-- A reordering of encoded rows is the encoding of a reordering. -/
theorem perm_of_map_inj {α β : Type} (f : α → β) (hf : Function.Injective f) (R : List β) (l : List α)
    (h : R.Perm (l.map f)) : ∃ l' : List α, R = l'.map f ∧ l'.Perm l := by
  rcases l with _ | ⟨x, xs⟩
  · simp only [List.map_nil, List.perm_nil] at h
    exact ⟨[], by simp [h], .refl _⟩
  · haveI : Nonempty α := ⟨x⟩
    refine ⟨R.map (Function.invFun f), ?_, ?_⟩
    · rw [List.map_map]
      conv_lhs => rw [← List.map_id R]
      refine List.map_congr_left (fun r hr => ?_)
      obtain ⟨y, _, rfl⟩ := List.mem_map.1 (h.subset hr)
      exact (Function.invFun_eq ⟨y, rfl⟩).symm
    · have := h.map (Function.invFun f)
      rwa [List.map_map, Function.invFun_comp hf, List.map_id] at this

/-- Rows that each encode some value are the encoding of a list. -/
theorem exists_map {α β : Type} (f : α → β) (R : List β) (h : ∀ r ∈ R, ∃ x, r = f x) :
    ∃ l : List α, R = l.map f := by
  induction R with
  | nil => exact ⟨[], rfl⟩
  | cons r R ih =>
    obtain ⟨x, rfl⟩ := h r (by simp)
    obtain ⟨l, hl⟩ := ih (fun r' hr => h r' (by simp [hr]))
    exact ⟨x :: l, by simp [hl]⟩

/-- An upsert on encoded rows is the encoding of an upsert, when the key
columns say exactly what the key says. -/
theorem map_upsert {α β κ : Type} [DecidableEq κ] (key : α → κ) (f : α → List β) (n : Nat) (x : α) (l : List α)
    (h : ∀ y, (f y).take n = (f x).take n ↔ key y = key x) :
    upsert (·.take n) (f x) (l.map f) = (upsert key x l).map f := by
  induction l with
  | nil => rfl
  | cons y ys ih =>
    by_cases hk : key y = key x
    · simp [upsert, hk, (h y).2 hk]
    · have : ¬ (f y).take n = (f x).take n := fun e => hk ((h y).1 e)
      simp only [List.map_cons, upsert, this, hk, if_false, ih]

/-- A filter on encoded rows is the encoding of a filter. -/
theorem map_filter_of {α β : Type} (f : α → β) (p : β → Bool) (q : α → Bool) (l : List α) (h : ∀ x, p (f x) = q x) :
    (l.map f).filter p = (l.filter q).map f := by
  rw [List.filter_map]; congr 1; exact List.filter_congr (fun x _ => h x)

/-- A keyed table's loaded rows: a reordering of its rows (a fresh tenant's
keyed table is empty either way). -/
theorem keyed_perm {R E : List (List V)} (h : R.Perm E ∨ (R = [] ∧ E = [])) : R.Perm E := by
  rcases h with h | ⟨rfl, rfl⟩
  · exact h
  · exact .refl _

/-- A singleton table's loaded rows: its one row, or none while the state
still has the fresh row `f z`. -/
theorem one_row {α : Type} (f : α → List V) (z : α) (R E : List (List V)) (hE : E.length = 1)
    (hrow : ∀ r ∈ E, ∃ x, r = f x) (h : R.Perm E ∨ (R = [] ∧ E = [f z])) :
    ∃ l : List α, l.length ≤ 1 ∧ R = l.map f ∧ E = [f (l.headD z)] := by
  rcases h with h | ⟨rfl, rfl⟩
  · obtain ⟨e, rfl⟩ := List.length_eq_one_iff.1 hE
    obtain ⟨x, rfl⟩ := hrow e (by simp)
    exact ⟨[x], by simp, List.perm_singleton.1 h, rfl⟩
  · exact ⟨[], by simp, rfl, rfl⟩

theorem wellKeyed_perm (kl : Nat → Nat) (tabs tabs' : Tables V) (hk : WellKeyed kl tabs)
    (hp : ∀ t, (tabs' t).Perm (tabs t)) : WellKeyed kl tabs' ∧ readBack kl tabs' = readBack kl tabs := by
  refine ⟨fun t => ⟨((hp t).map _).nodup_iff.2 (hk t).1, fun r hr => (hk t).2 r ((hp t).subset hr)⟩, ?_⟩
  funext t k
  simp only [readBack]
  exact find_perm (·.take (kl t)) (hp t) (((hp t).map _).nodup_iff.2 (hk t).1) k

/-! ## The database holds the tables -/

section Holds
variable (kl : Nat → Nat)

theorem holds_fresh (I : Tables V) : Holds kl I (fun _ _ => none) I := by
  refine ⟨fun _ => [], ?_, fun t => by simp, fun t => .inr ⟨rfl, rfl⟩⟩
  funext t k; simp [readBack]

/-- The read-back database of well-keyed tables, run through the planned
statements of well-formed writes: the tables those writes produce. -/
theorem holds_step (I : Tables V) (hI : InitOk kl I) (db : Db V) (E : Tables V) (ws : List (AWrite V))
    (h : Holds kl I db E) (hok : ∀ a ∈ ws, WriteOk kl a) :
    Holds kl I (execAll db (ws.map planA)) (applyAllW kl E ws) := by
  obtain ⟨tabs, rfl, hk, he⟩ := h
  obtain ⟨h1, h2⟩ := plan_sound kl ws tabs hk hok
  refine ⟨applyAllW kl tabs ws, h1.symm, h2, fun t => ?_⟩
  rcases he t with e | ⟨e0, eI⟩
  · exact .inl (applyAllW_at kl ws _ _ t e)
  · rcases hI t with hI0 | ⟨hkl, hlen⟩
    · exact .inl (applyAllW_at kl ws _ _ t (by rw [e0, eI, hI0]))
    · by_cases ht : ∃ a ∈ ws, target a = t
      · exact .inl (applyAllW_single kl t hkl ws hok ht _ _ (by simp [e0]) (by rw [eI, hlen]))
      · have hn : ∀ a ∈ ws, target a ≠ t := fun a ha hat => ht ⟨a, ha, hat⟩
        right
        rw [applyAllW_other kl ws t hn, applyAllW_other kl ws t hn]
        exact ⟨e0, eI⟩

/-- Row order does not matter. -/
theorem holds_perm (I : Tables V) (hI : InitOk kl I) (db : Db V) (E E' : Tables V)
    (h : Holds kl I db E) (hp : ∀ t, (E' t).Perm (E t)) : Holds kl I db E' := by
  obtain ⟨tabs, rfl, hk, he⟩ := h
  have hpt : ∀ t, ((fun t => if tabs t = E t then E' t else tabs t) t).Perm (tabs t) := fun t => by
    by_cases h : tabs t = E t
    · simp only [h, if_true]; exact hp t
    · simp only [h, if_false]; exact .refl _
  obtain ⟨hk', hr⟩ := wellKeyed_perm kl tabs _ hk hpt
  refine ⟨_, hr.symm, hk', fun t => ?_⟩
  by_cases hne : tabs t = E t
  · simp [hne]
  · simp only [hne, if_false]
    rcases he t with e | ⟨e0, eI⟩
    · exact absurd e hne
    · refine .inr ⟨e0, ?_⟩
      have hpe := hp t
      rw [eI] at hpe
      rcases hI t with hI0 | ⟨-, hlen⟩
      · rw [hI0] at hpe ⊢; exact List.perm_nil.1 hpe
      · obtain ⟨d, hd⟩ : ∃ d, I t = [d] := List.length_eq_one_iff.1 hlen
        rw [hd] at hpe ⊢
        exact List.perm_singleton.1 hpe

/-- The rows a tenant-filtered `SELECT` returns from a database holding `E`:
each table's rows reordered, or none where `E` still has its fresh rows. -/
theorem holds_lists (I : Tables V) (db : Db V) (E R : Tables V) (h : Holds kl I db E) (hl : Lists kl db R) :
    ∀ t, (R t).Perm (E t) ∨ (R t = [] ∧ E t = I t) := by
  obtain ⟨tabs, rfl, hk, he⟩ := h
  intro t
  have hp : (R t).Perm (tabs t) := by
    rw [List.perm_ext_iff_of_nodup (hl t).1 (List.Nodup.of_map _ (hk t).1)]
    intro row
    rw [(hl t).2]
    simp only [readBack]
    rw [find_key_iff (fun r : List V => r.take (kl t)) (tabs t) (hk t).1 _ row]
    simp
  rcases he t with e | ⟨e0, eI⟩
  · exact .inl (e ▸ hp)
  · exact .inr ⟨List.perm_nil.1 (e0 ▸ hp), eI⟩

end Holds

/-! ## An app's storage -/

/-- A put writes a row of its table's type. -/
def RowOk (IsRow : Nat → List V → Prop) : AWrite V → Prop
  | .put t _ row => IsRow t row
  | _ => True

/-- An app's storage: its state's encoding as tables, the table writes of each
kernel write, and what a kernel write does to the state. `IsRow t row` says
`row` encodes a value of table `t`'s row type. -/
structure App (St W V : Type) where
  kl : Nat → Nat
  enc : St → Tables V
  sql : W → List (AWrite V)
  step : St → W → St
  init : St
  IsRow : Nat → List V → Prop
  /-- The table writes of `w` turn the encoding of `s` into the encoding of
  `step s w`. -/
  enc_step : ∀ s w, applyAllW kl (enc s) (sql w) = enc (step s w)
  sql_ok : ∀ w, ∀ a ∈ sql w, WriteOk kl a ∧ RowOk IsRow a
  init_ok : InitOk kl (enc init)
  init_rows : ∀ t, ∀ r ∈ enc init t, IsRow t r

namespace App
variable {St W : Type} (A : App St W V)

/-- The database holds `E`, for a tenant that started fresh. -/
def Holds (db : Db V) (E : Tables V) : Prop := Store.Holds A.kl (A.enc A.init) db E

/-- Two states whose tables hold the same rows, maybe in another order. -/
def Equiv (s s' : St) : Prop := ∀ t, (A.enc s t).Perm (A.enc s' t)

/-- Every row encodes a row of its table, and a table without key columns
keeps the one row it started with. -/
def Shaped (E : Tables V) : Prop :=
  (∀ t, ∀ r ∈ E t, A.IsRow t r) ∧ ∀ t, A.kl t = 0 → (A.enc A.init t).length = 1 → (E t).length = 1

/-- The table writes of a write set, in order. -/
def sqlAll (ws : List W) : List (AWrite V) := ws.flatMap A.sql

theorem applyAll_enc (s : St) (ws : List W) :
    applyAllW A.kl (A.enc s) (A.sqlAll ws) = A.enc (ws.foldl A.step s) := by
  induction ws generalizing s with
  | nil => rfl
  | cons w ws ih =>
    have e : applyAllW A.kl (A.enc s) (A.sqlAll (w :: ws)) =
        applyAllW A.kl (applyAllW A.kl (A.enc s) (A.sql w)) (A.sqlAll ws) := by
      simp only [sqlAll, List.flatMap_cons, applyAllW, List.foldl_append]
    rw [e, A.enc_step, ih]
    rfl

theorem sqlAll_ok (ws : List W) : ∀ a ∈ A.sqlAll ws, WriteOk A.kl a ∧ RowOk A.IsRow a := by
  intro a ha
  obtain ⟨w, _, hw⟩ := List.mem_flatMap.1 ha
  exact A.sql_ok w a hw

theorem shaped_applyW (E : Tables V) (a : AWrite V) (hE : A.Shaped E) (ha : WriteOk A.kl a ∧ RowOk A.IsRow a) :
    A.Shaped (applyW A.kl E a) := by
  obtain ⟨hr, hs⟩ := hE
  refine ⟨fun t r hrt => ?_, fun t hkl hlen => ?_⟩
  · cases a with
    | put t0 n row =>
      simp only [applyW] at hrt
      split at hrt
      · subst_vars
        rcases mem_upsert_of hrt with rfl | h
        · exact ha.2
        · exact hr _ r h
      · exact hr t r hrt
    | del t0 k =>
      simp only [applyW] at hrt
      split at hrt
      · subst_vars; exact hr _ r (List.mem_filter.1 hrt).1
      · exact hr t r hrt
    | delWhere t0 i v =>
      simp only [applyW] at hrt
      split at hrt
      · subst_vars; exact hr _ r (List.mem_filter.1 hrt).1
      · exact hr t r hrt
  · have h1 := hs t hkl hlen
    by_cases ht : target a = t
    · cases a with
      | put t0 n row =>
        simp only [target] at ht; subst ht
        simp only [applyW, if_true, hkl]
        rw [upsert_single row _ (by omega)]; rfl
      | del t0 k => simp only [target] at ht; subst ht; simp [WriteOk, hkl] at ha
      | delWhere t0 i v => simp only [target] at ht; subst ht; simp [WriteOk, hkl] at ha
    · rw [applyW_other A.kl E a t ht]; exact h1

theorem shaped_applyAllW (ws : List (AWrite V)) (hws : ∀ a ∈ ws, WriteOk A.kl a ∧ RowOk A.IsRow a) :
    ∀ E, A.Shaped E → A.Shaped (applyAllW A.kl E ws) := by
  induction ws with
  | nil => intro E h; exact h
  | cons a ws ih =>
    intro E h
    exact ih (fun b hb => hws b (by simp [hb])) _ (A.shaped_applyW E a h (hws a (by simp)))

theorem shaped_perm (E E' : Tables V) (h : A.Shaped E) (hp : ∀ t, (E' t).Perm (E t)) : A.Shaped E' :=
  ⟨fun t r hr => h.1 t r ((hp t).subset hr), fun t hkl hlen => (hp t).length_eq.trans (h.2 t hkl hlen)⟩

theorem shaped_init : A.Shaped (A.enc A.init) :=
  ⟨A.init_rows, fun _ _ h => h⟩

/-- Databases the store produces from a fresh tenant, with the state each holds.
A commit loads `s'` (the stored tables up to row order) and runs the plan of `ws`. -/
inductive Served : Db V → St → Prop
  | fresh : Served (fun _ _ => none) A.init
  | commit {db : Db V} {s s' : St} {ws : List W} :
      Served db s → A.Equiv s' s → Served (execAll db ((A.sqlAll ws).map planA)) (ws.foldl A.step s')

/-- After any sequence of commits, the database reads back exactly the
encoding of the state that applying the write sets computes. -/
theorem served_holds {db : Db V} {s : St} (h : A.Served db s) : A.Holds db (A.enc s) ∧ A.Shaped (A.enc s) := by
  induction h with
  | fresh => exact ⟨holds_fresh A.kl _, A.shaped_init⟩
  | @commit db s s' ws _ he ih =>
    obtain ⟨hh, hs⟩ := ih
    have hh' := holds_perm A.kl _ A.init_ok db _ _ hh he
    have hs' := A.shaped_perm _ _ hs he
    rw [← A.applyAll_enc]
    exact ⟨holds_step A.kl _ A.init_ok db _ _ hh' (fun a ha => (A.sqlAll_ok ws a ha).1),
      A.shaped_applyAllW _ (A.sqlAll_ok ws) _ hs'⟩

/-- A `SELECT` of every table returns each table's encoding up to row order, or
nothing where the state still has the fresh rows. -/
theorem served_lists {db : Db V} {s : St} (h : A.Served db s) (R : Tables V) (hl : Lists A.kl db R) :
    ∀ t, (R t).Perm (A.enc s t) ∨ (R t = [] ∧ A.enc s t = A.enc A.init t) :=
  holds_lists A.kl _ db _ R (A.served_holds h).1 hl

end App

end H5iAppLib.Store
