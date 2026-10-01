import Storage
import Invariants
/-!
# What the store reads back (A4, load side)

Round trip of `decode` for every row type, then the database invariant
`DbInv` in any row order: `load_sound` for loading, `store_sound` for storing.
-/
open Aeneas Aeneas.Std Result docs_kernel docs_kernel.Spec H5iAppLib H5iAppLib.Sql docs_kernel.Storage docs_kernel.Schema

namespace docs_kernel.Load

/-! ## A table decodes what it encoded -/

/-- The body of every generated `from_rows` loop, for its row decoder `F`. -/
def rowsBody {T : Type} (F : alloc.vec.Vec Val → Result (Option T))
    (rows : alloc.vec.Vec (alloc.vec.Vec Val)) (out : alloc.vec.Vec T) (ok1 : Bool) (i : Usize) :
    Result (ControlFlow ((alloc.vec.Vec T) × Bool × Usize) ((alloc.vec.Vec T) × Bool)) := do
  let i1 := alloc.vec.Vec.len rows
  if i < i1
  then
    let v ← alloc.vec.Vec.index (core.slice.index.SliceIndexUsizeSlice (alloc.vec.Vec Val)) rows i
    let o ← F v
    let (out1, ok2) ←
      match o with
      | none => ok (out, false)
      | some x => do
                  let out2 ← alloc.vec.Vec.push out x
                  ok (out2, ok1)
    let i2 ← i + 1#usize
    ok (ControlFlow.cont (out1, ok2, i2))
  else ok (ControlFlow.done (out, ok1))

theorem rows_loop {T : Type} (F : alloc.vec.Vec Val → Result (Option T)) (f : T → List Val)
    (hF : ∀ x v, v.val = f x → F v ⦃ o => o = some x ⦄)
    (rows : alloc.vec.Vec (alloc.vec.Vec Val)) (l : List T) (h : rows.val.map (·.val) = l.map f) :
    loop (fun (x : alloc.vec.Vec T × Bool × Usize) => rowsBody F rows x.1 x.2.1 x.2.2)
      (alloc.vec.Vec.new T, true, 0#usize) ⦃ r => r.2 = true ∧ r.1.val = l ⦄ := by
  have hlen : rows.length = l.length := by
    simpa [alloc.vec.Vec.length] using congrArg List.length h
  apply loop.spec_decr_nat
    (measure := fun (x : alloc.vec.Vec T × Bool × Usize) => rows.length - x.2.2.val)
    (inv := fun x => x.2.2.val ≤ rows.length ∧ x.2.1 = true ∧ x.1.val = l.take x.2.2.val)
  · rintro ⟨o, b, j⟩ ⟨hj, hb, heq⟩
    simp only at hj hb heq ⊢
    subst hb
    unfold rowsBody
    dsimp only
    split
    · have hj' : j.val < l.length := by scalar_tac
      step
      have hv : v.val = f l[j.val] := by
        have := congrArg (·[j.val]?) h
        simp [List.getElem?_map, List.getElem?_eq_getElem hj', List.getElem?_eq_getElem (show j.val < rows.val.length by scalar_tac)] at this
        rw [v_post, this]
      step with hF _ _ hv as ⟨ o, ho ⟩
      rw [ho]
      step*
      refine ⟨by scalar_tac, ?_, by scalar_tac⟩
      rw [x_post, i2_post, heq, List.take_add_one, List.getElem?_eq_getElem hj']
      rfl
    · simp only [WP.spec_ok]
      refine ⟨trivial, ?_⟩
      rw [heq, List.take_of_length_le (by scalar_tac)]
  · simp

theorem project_from_rows (rows : alloc.vec.Vec (alloc.vec.Vec Val)) (l : List Project)
    (h : rows.val.map (·.val) = l.map Project.row) :
    Project.from_rows rows ⦃ o => ∃ v, o = some v ∧ v.val = l ⦄ := by
  have e : Project.from_rows_loop rows (alloc.vec.Vec.new Project) true 0#usize =
      loop (fun x => rowsBody Project.from_row rows x.1 x.2.1 x.2.2) (alloc.vec.Vec.new Project, true, 0#usize) := by
    unfold Project.from_rows_loop; congr 1; funext ⟨a, b, c⟩
    simp only []; unfold Project.from_rows_loop.body rowsBody
    dsimp only
    split <;> (try rfl)
    congr 1; funext v; congr 1; funext o; cases o <;> rfl
  have hl : Project.from_rows_loop rows (alloc.vec.Vec.new Project) true 0#usize ⦃ r => r.2 = true ∧ r.1.val = l ⦄ := by
    rw [e]; exact rows_loop Project.from_row Project.row (fun x v hv => Project.from_row_spec x v hv) rows l h
  unfold Project.from_rows
  step with hl
  simp [ok1_post, l_post]

theorem member_from_rows (rows : alloc.vec.Vec (alloc.vec.Vec Val)) (l : List Member)
    (h : rows.val.map (·.val) = l.map Member.row) :
    Member.from_rows rows ⦃ o => ∃ v, o = some v ∧ v.val = l ⦄ := by
  have e : Member.from_rows_loop rows (alloc.vec.Vec.new Member) true 0#usize =
      loop (fun x => rowsBody Member.from_row rows x.1 x.2.1 x.2.2) (alloc.vec.Vec.new Member, true, 0#usize) := by
    unfold Member.from_rows_loop; congr 1; funext ⟨a, b, c⟩
    simp only []; unfold Member.from_rows_loop.body rowsBody
    dsimp only
    split <;> (try rfl)
    congr 1; funext v; congr 1; funext o; cases o <;> rfl
  have hl : Member.from_rows_loop rows (alloc.vec.Vec.new Member) true 0#usize ⦃ r => r.2 = true ∧ r.1.val = l ⦄ := by
    rw [e]; exact rows_loop Member.from_row Member.row (fun x v hv => Member.from_row_spec x v hv) rows l h
  unfold Member.from_rows
  step with hl
  simp [ok1_post, l_post]

theorem document_from_rows (rows : alloc.vec.Vec (alloc.vec.Vec Val)) (l : List Document)
    (h : rows.val.map (·.val) = l.map Document.row) :
    Document.from_rows rows ⦃ o => ∃ v, o = some v ∧ v.val = l ⦄ := by
  have e : Document.from_rows_loop rows (alloc.vec.Vec.new Document) true 0#usize =
      loop (fun x => rowsBody Document.from_row rows x.1 x.2.1 x.2.2) (alloc.vec.Vec.new Document, true, 0#usize) := by
    unfold Document.from_rows_loop; congr 1; funext ⟨a, b, c⟩
    simp only []; unfold Document.from_rows_loop.body rowsBody
    dsimp only
    split <;> (try rfl)
    congr 1; funext v; congr 1; funext o; cases o <;> rfl
  have hl : Document.from_rows_loop rows (alloc.vec.Vec.new Document) true 0#usize ⦃ r => r.2 = true ∧ r.1.val = l ⦄ := by
    rw [e]; exact rows_loop Document.from_row Document.row (fun x v hv => Document.from_row_spec x v hv) rows l h
  unfold Document.from_rows
  step with hl
  simp [ok1_post, l_post]

theorem webhook_from_rows (rows : alloc.vec.Vec (alloc.vec.Vec Val)) (l : List Webhook)
    (h : rows.val.map (·.val) = l.map Webhook.row) :
    Webhook.from_rows rows ⦃ o => ∃ v, o = some v ∧ v.val = l ⦄ := by
  have e : Webhook.from_rows_loop rows (alloc.vec.Vec.new Webhook) true 0#usize =
      loop (fun x => rowsBody Webhook.from_row rows x.1 x.2.1 x.2.2) (alloc.vec.Vec.new Webhook, true, 0#usize) := by
    unfold Webhook.from_rows_loop; congr 1; funext ⟨a, b, c⟩
    simp only []; unfold Webhook.from_rows_loop.body rowsBody
    dsimp only
    split <;> (try rfl)
    congr 1; funext v; congr 1; funext o; cases o <;> rfl
  have hl : Webhook.from_rows_loop rows (alloc.vec.Vec.new Webhook) true 0#usize ⦃ r => r.2 = true ∧ r.1.val = l ⦄ := by
    rw [e]; exact rows_loop Webhook.from_row Webhook.row (fun x v hv => Webhook.from_row_spec x v hv) rows l h
  unfold Webhook.from_rows
  step with hl
  simp [ok1_post, l_post]

/-! ## Lists with unique keys -/

theorem find_key_iff {α κ : Type} {_ : DecidableEq κ} (key : α → κ) (l : List α) (hn : (l.map key).Nodup)
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
theorem find_perm {α κ : Type} {_ : DecidableEq κ} (key : α → κ) {l l' : List α} (hp : l.Perm l')
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

/-! ## The database invariant -/

/-- The rows of `s`; `c` says whether the counter row exists yet. -/
def encC (c : Bool) (s : St) : Tables Val := fun t =>
  if t = 3 then (if c then [counterRow s.next] else []) else enc s t

/-- The tenant's rows hold `s`. A fresh tenant has no counter row. -/
def Stored (db : Db Val) (s : St) : Prop :=
  ∃ c : Bool, (c = false → s.next = 0) ∧ s.next < 2 ^ 64 ∧ db = readBack kl (encC c s)

/-- The tenant's rows hold some state satisfying `Inv`. -/
def DbInv (db : Db Val) : Prop := ∃ s, Inv s ∧ Stored db s

def setsCounter : Write → Bool
  | .SetCounter _ => true
  | _ => false

theorem fresh : DbInv (fun _ _ => none) := by
  refine ⟨init, Theorems.init_inv, false, fun _ => rfl, by simp [init], ?_⟩
  funext t k
  rcases t with _ | _ | _ | _ | _ | t <;> simp [readBack, encC, enc, init]

theorem wellKeyedC (c : Bool) (s : St) (h : Inv s) : WellKeyed kl (encC c s) := by
  intro t
  by_cases ht : t = 3
  · subst ht; cases c <;> simp [encC, kl, counterRow]
  · simpa [encC, ht] using wellKeyed s h t

theorem applyW_pt (tabs tabs' : Tables Val) (a : AWrite Val) (t : Nat) (h : tabs t = tabs' t) :
    applyW kl tabs a t = applyW kl tabs' a t := by
  cases a <;> simp only [applyW] <;> split <;> simp_all

theorem encodeC_step (c : Bool) (s : St) (w : Write) (a : AWrite Val) (ha : sqlA w = some a) :
    applyW kl (encC c s) a = encC (c || setsCounter w) (applyWrite s w) := by
  funext t
  by_cases ht : t = 3
  · subst ht
    cases w <;> simp only [sqlA, Option.some.injEq, reduceCtorEq] at ha <;> subst ha <;>
      cases c <;> simp [applyW, encC, applyWrite, setsCounter, upsert, kl]
  · have e := congrFun (encode_step s w) t
    rw [ha] at e
    simp only at e
    rw [applyW_pt _ (enc s) _ _ (by simp [encC, ht])]
    simp [encC, ht, e]

theorem encodeC_applyAll (c : Bool) (s : St) (ws : List Write) :
    applyAllW kl (encC c s) (ws.filterMap sqlA) = encC (c || ws.any setsCounter) (applyAll s ws) := by
  induction ws generalizing c s with
  | nil => simp [applyAllW, applyAll]
  | cons w ws ih =>
    simp only [applyAll, List.foldl_cons, List.filterMap_cons, List.any_cons] at ih ⊢
    cases h : sqlA w with
    | none =>
      cases w <;> simp only [sqlA, reduceCtorEq] at h
      simp only [applyWrite, setsCounter, Bool.false_or]
      exact ih c s
    | some a =>
      simp only [applyAllW, List.foldl_cons] at ih ⊢
      rw [encodeC_step c s w a h, ih, Bool.or_assoc]

theorem next_keep (s : St) (ws : List Write) (h : ws.any setsCounter = false) :
    (applyAll s ws).next = s.next := by
  induction ws generalizing s with
  | nil => rfl
  | cons w ws ih =>
    simp only [List.any_cons, Bool.or_eq_false_iff] at h
    simp only [applyAll, List.foldl_cons] at ih ⊢
    rw [ih _ h.2]
    cases w <;> simp_all [applyWrite, setsCounter]

theorem next_bound (s : St) (ws : List Write) (h : s.next < 2 ^ 64) : (applyAll s ws).next < 2 ^ 64 := by
  induction ws generalizing s with
  | nil => exact h
  | cons w ws ih =>
    simp only [applyAll, List.foldl_cons] at ih ⊢
    apply ih
    cases w <;> simp_all [applyWrite]
    rename_i c; have := c.next_id.hBounds; simpa using this

/-- Storing a write set keeps the rows in step with the state. -/
theorem stored_step (db : Db Val) (s : St) (ws : List Write) (hi : Inv s) (hs : Stored db s) :
    Stored (execAll db ((ws.filterMap sqlA).map planA)) (applyAll s ws) := by
  obtain ⟨c, hc, hb, rfl⟩ := hs
  refine ⟨c || ws.any setsCounter, fun h => ?_, next_bound s ws hb, ?_⟩
  · simp only [Bool.or_eq_false_iff] at h
    rw [next_keep s ws h.2, hc h.1]
  · rw [← encodeC_applyAll]
    refine ((plan_sound kl _ _ (wellKeyedC c s hi) ?_).1).symm
    intro a ha
    obtain ⟨w, _, hw⟩ := List.mem_filterMap.1 ha
    exact writeOk w a hw

/-- `Storage.sql_writes_stored`, also for a tenant with no counter row yet
(`c = false`). -/
theorem sql_writes_storedC (c : Bool) (s : St) (h : Inv s) (ws : alloc.vec.Vec Write) :
    sql_writes ws ⦃ v =>
      execAll (readBack kl (encC c s)) ((v.val.map Write.abs).map planA) =
        readBack kl (encC (c || ws.val.any setsCounter) (applyAll s ws.val)) ⦄ := by
  apply WP.spec_mono (sql_writes_spec ws)
  intro v hv
  rw [hv, ← encodeC_applyAll]
  refine ((plan_sound kl _ _ (wellKeyedC c s h) ?_).1).symm
  intro a ha
  obtain ⟨w, _, hw⟩ := List.mem_filterMap.1 ha
  exact writeOk w a hw

/-! ## Loading -/

/-- The loader returns every stored row of each table exactly once, in any
order: what the compiled `SELECT`s return (`Database.lists_of`). -/
def Lists (db : Db Val) (R : Nat → List (List Val)) : Prop :=
  ∀ t, (R t).Nodup ∧ ∀ row, row ∈ R t ↔ db t (row.take (kl t)) = some row

def rowsOf (r : Rows) : Nat → List (List Val)
  | 0 => r.projects.val.map (·.val)
  | 1 => r.members.val.map (·.val)
  | 2 => r.documents.val.map (·.val)
  | 3 => r.counter.val.map (·.val)
  | 4 => r.webhooks.val.map (·.val)
  | _ => []

theorem loaded_perm (db : Db Val) (s : St) (c : Bool) (hi : Inv s) (hdb : db = readBack kl (encC c s))
    (R : Nat → List (List Val)) (hl : Lists db R) (t : Nat) : (R t).Perm (encC c s t) := by
  have hk := (wellKeyedC c s hi t).1
  rw [List.perm_ext_iff_of_nodup (hl t).1 (List.Nodup.of_map _ hk)]
  intro row
  rw [(hl t).2, hdb]
  simp only [readBack]
  exact (find_key_iff (fun r : List Val => r.take (kl t)) (encC c s t) hk _ row).trans (by simp)

/-- Same counter, same rows per table, maybe in another order. -/
structure Equiv (s s' : St) : Prop where
  next : s'.next = s.next
  projects : s'.projects.Perm s.projects
  members : s'.members.Perm s.members
  docs : s'.docs.Perm s.docs
  webhooks : s'.webhooks.Perm s.webhooks

theorem owners_perm {ms ms' : List Member} (h : ms'.Perm ms) (p : Nat) : owners ms' p = owners ms p :=
  (h.filter _).length_eq

theorem inv_equiv {s s' : St} (e : Equiv s s') (hi : Inv s) : Inv s' where
  owned p hp := by rw [owners_perm e.members]; exact hi.owned p (e.projects.subset hp)
  member_proj m hm := by
    obtain ⟨p, hp, h⟩ := hi.member_proj m (e.members.subset hm); exact ⟨p, e.projects.symm.subset hp, h⟩
  doc_proj d hd := by
    obtain ⟨p, hp, h⟩ := hi.doc_proj d (e.docs.subset hd); exact ⟨p, e.projects.symm.subset hp, h⟩
  proj_keys := (e.projects.map _).nodup_iff.2 hi.proj_keys
  member_keys := (e.members.map _).nodup_iff.2 hi.member_keys
  doc_keys := (e.docs.map _).nodup_iff.2 hi.doc_keys
  proj_fresh p hp := by rw [e.next]; exact hi.proj_fresh p (e.projects.subset hp)
  doc_fresh d hd := by rw [e.next]; exact hi.doc_fresh d (e.docs.subset hd)
  four_eyes d hd := hi.four_eyes d (e.docs.subset hd)
  approver_iff d hd := hi.approver_iff d (e.docs.subset hd)
  hook_proj w hw := by
    obtain ⟨p, hp, h⟩ := hi.hook_proj w (e.webhooks.subset hw); exact ⟨p, e.projects.symm.subset hp, h⟩
  hook_keys := (e.webhooks.map _).nodup_iff.2 hi.hook_keys

theorem encC_perm {s s' : St} (e : Equiv s s') (c : Bool) (t : Nat) : (encC c s' t).Perm (encC c s t) := by
  rcases t with _ | _ | _ | _ | _ | t
  · exact e.projects.map _
  · exact e.members.map _
  · exact e.docs.map _
  · simp [encC, e.next]
  · exact e.webhooks.map _
  · simp [encC, enc]

theorem stored_equiv {db : Db Val} {s s' : St} (e : Equiv s s') (hi : Inv s) (hs : Stored db s) :
    Stored db s' := by
  obtain ⟨c, hc, hb, rfl⟩ := hs
  refine ⟨c, fun h => by rw [e.next]; exact hc h, by rw [e.next]; exact hb, ?_⟩
  funext t k
  simp only [readBack]
  exact find_perm (·.take (kl t)) (encC_perm e c t).symm (wellKeyedC c s hi t).1 k

/-- Decoding rows whose tables encode `lp`, `lm`, `ld`, `lw` (in that order)
and whose counter table holds `s`'s counter. -/
theorem decode_lists (r : Rows) (s : St) (c : Bool) (hc : c = false → s.next = 0) (hb : s.next < 2 ^ 64)
    (h3 : (r.counter.val.map (·.val)).Perm (if c then [counterRow s.next] else []))
    (lp : List Project) (ep : r.projects.val.map (·.val) = lp.map Project.row)
    (lm : List Member) (em : r.members.val.map (·.val) = lm.map Member.row)
    (ld : List Document) (ed : r.documents.val.map (·.val) = ld.map Document.row)
    (lw : List Webhook) (ew : r.webhooks.val.map (·.val) = lw.map Webhook.row) :
    decode r ⦃ o => ∃ snap, o = some snap ∧ snap.counter.next_id.val = s.next ∧
      snap.projects.val = lp ∧ snap.members.val = lm ∧ snap.documents.val = ld ∧ snap.webhooks.val = lw ⦄ := by
  let cnt : Counter := { next_id := ⟨BitVec.ofNat _ s.next⟩ }
  have hcnt : cnt.next_id.val = s.next := by
    simp only [cnt, UScalar.val, BitVec.toNat_ofNat]
    exact Nat.mod_eq_of_lt (by simpa using hb)
  unfold decode
  have hctr : (if r.counter.len = 0#usize then ok (some ({ next_id := 0#u64 } : Counter))
      else do
        let v ← r.counter.index_usize 0#usize
        Counter.from_row v) ⦃ o => ∃ k, o = some k ∧ k.next_id.val = s.next ⦄ := by
    cases c with
    | false =>
      simp only [Bool.false_eq_true, if_false, List.perm_nil, List.map_eq_nil_iff] at h3
      have : r.counter.len = 0#usize := by scalar_tac
      simp [this, hc rfl]
    | true =>
      simp only [if_true, List.perm_singleton, List.map_eq_singleton_iff] at h3
      obtain ⟨v, hv, hvv⟩ := h3
      have hne : r.counter.len ≠ 0#usize := by scalar_tac
      simp only [hne, if_false]
      step as ⟨ w, hw ⟩
      have hw' : w.val = counterRow cnt.next_id.val := by rw [hcnt, hw]; simp [hv, hvv]
      step with Counter.from_row_spec cnt w (by rw [Counter.row_eq]; exact hw') as ⟨ o, ho ⟩
      exact ⟨cnt, ho, hcnt⟩
  step with hctr as ⟨ o, k, hok, hk ⟩
  step with project_from_rows r.projects lp ep as ⟨ o0, vp, h0v, hvp ⟩
  step with member_from_rows r.members lm em as ⟨ o1, vm, h1v, hvm ⟩
  step with document_from_rows r.documents ld ed as ⟨ o2, vd, h2v, hvd ⟩
  step with webhook_from_rows r.webhooks lw ew as ⟨ o3, vw, h3v, hvw ⟩
  subst hok h0v h1v h2v h3v
  simp only [WP.spec_ok]
  exact ⟨_, rfl, hk, hvp, hvm, hvd, hvw⟩

theorem decode_spec (r : Rows) (s : St) (c : Bool) (hc : c = false → s.next = 0) (hb : s.next < 2 ^ 64)
    (hp : ∀ t, (rowsOf r t).Perm (encC c s t)) :
    decode r ⦃ o => ∃ snap, o = some snap ∧ Equiv s (Snapshot.toSt snap) ⦄ := by
  have h0 : (r.projects.val.map (·.val)).Perm (s.projects.map Project.row) := by simpa [rowsOf, encC, enc] using hp 0
  have h1 : (r.members.val.map (·.val)).Perm (s.members.map Member.row) := by simpa [rowsOf, encC, enc] using hp 1
  have h2 : (r.documents.val.map (·.val)).Perm (s.docs.map Document.row) := by simpa [rowsOf, encC, enc] using hp 2
  have h4 : (r.webhooks.val.map (·.val)).Perm (s.webhooks.map Webhook.row) := by simpa [rowsOf, encC, enc] using hp 4
  have h3 : (r.counter.val.map (·.val)).Perm (if c then [counterRow s.next] else []) := by
    simpa [rowsOf, encC] using hp 3
  obtain ⟨lp, ep, pp⟩ := perm_of_map_inj _ Project.row_inj _ _ h0
  obtain ⟨lm, em, pm⟩ := perm_of_map_inj _ Member.row_inj _ _ h1
  obtain ⟨ld, ed, pd⟩ := perm_of_map_inj _ Document.row_inj _ _ h2
  obtain ⟨lw, ew, pw⟩ := perm_of_map_inj _ Webhook.row_inj _ _ h4
  apply WP.spec_mono (decode_lists r s c hc hb h3 lp ep lm em ld ed lw ew)
  rintro o ⟨snap, rfl, hk, hvp, hvm, hvd, hvw⟩
  exact ⟨snap, rfl, ⟨hk, by simp [Snapshot.toSt, hvp, pp], by simp [Snapshot.toSt, hvm, pm],
    by simp [Snapshot.toSt, hvd, pd], by simp [Snapshot.toSt, hvw, pw]⟩⟩

/-! ## Main theorems -/

/-- Loading a tenant, in whatever row order the database uses, gives a
snapshot that satisfies `Inv` and that the rows hold. -/
theorem load_sound (db : Db Val) (hdb : DbInv db) (r : Rows) (hl : Lists db (rowsOf r)) :
    decode r ⦃ o => ∃ snap, o = some snap ∧ Inv (Snapshot.toSt snap) ∧ Stored db (Snapshot.toSt snap) ⦄ := by
  obtain ⟨s, hi, c, hc, hb, hdbeq⟩ := hdb
  apply WP.spec_mono (decode_spec r s c hc hb (loaded_perm db s c hi hdbeq _ hl))
  rintro o ⟨snap, rfl, e⟩
  exact ⟨snap, rfl, inv_equiv e hi, stored_equiv e hi ⟨c, hc, hb, hdbeq⟩⟩

/-- Storing a successful command's writes on a loaded snapshot keeps `DbInv`. -/
theorem store_sound (db : Db Val) (snap : Snapshot) (hi : Inv (Snapshot.toSt snap))
    (hs : Stored db (Snapshot.toSt snap)) (a : Principal) (c : Command) ws reply
    (h : transition a snap c = .ok (.Ok (ws, reply))) :
    sql_writes ws ⦃ v => DbInv (execAll db ((v.val.map Write.abs).map planA)) ⦄ := by
  apply WP.spec_mono (sql_writes_spec ws)
  intro v hv
  rw [hv]
  exact ⟨_, Theorems.inv_preserved a snap c ws reply hi h, stored_step db _ ws.val hi hs⟩

end docs_kernel.Load
