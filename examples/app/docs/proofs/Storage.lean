import Spec
import Schema
/-!
# What the store writes (A4)

The server stores a write set by running `h5i_app_sql::plan` on the kernel's own
`sql_writes`. We prove:

- `sql_writes_spec`: the extracted `sql_writes` computes `sqlA` on each write.
- `encode_applyAll`: applying those table writes to the rows of a state gives
  the rows of `applyAll`, the state every other theorem talks about.
- `stored`: running the planned statements on a database that holds a valid
  state leaves exactly the rows of the new state.

Statement semantics: `H5iAppLib.Sql`; `plan` = `planA` and the SQL text are
proven in `h5i-app-sql` and `h5i-app-pgsql`.
-/
open Aeneas Aeneas.Std Result docs_kernel docs_kernel.Spec H5iAppLib H5iAppLib.Sql docs_kernel.Schema

namespace docs_kernel.Storage

/-- The counter table's one row, for counter `n`. -/
def counterRow (n : Nat) : List Val := [int n]

@[simp] theorem Counter.row_eq (c : Counter) : Counter.row c = counterRow c.next_id.val := rfl

/-- The rows a state is stored as. -/
def enc (s : St) : Tables Val
  | 0 => s.projects.map Project.row
  | 1 => s.members.map Member.row
  | 2 => s.docs.map Document.row
  | 3 => [counterRow s.next]
  | 4 => s.webhooks.map Webhook.row
  | _ => []

/-- The table write for one kernel write; effects go to the outbox instead. -/
def sqlA : Write → Option (AWrite Val)
  | .PutProject p => some (.put 0 1 (Project.row p))
  | .PutMember m => some (.put 1 2 (Member.row m))
  | .DelMember p u => some (.del 1 [int p.val, int u.val])
  | .PutDocument d => some (.put 2 1 (Document.row d))
  | .DelDocument i => some (.del 2 [int i.val])
  | .SetCounter c => some (.put 3 0 (counterRow c.next_id.val))
  | .PutWebhook h => some (.put 4 1 (Webhook.row h))
  | .DelWebhook p => some (.del 4 [int p.val])
  | .Emit _ => none

/-! ## Row writes commute with encoding -/

theorem map_upsert {α β κ : Type} [DecidableEq κ] {_ : DecidableEq (List β)} (key : α → κ)
    (f : α → List β) (n : Nat) (x : α) (l : List α)
    (h : ∀ y, (f y).take n = (f x).take n ↔ key y = key x) :
    (upsert key x l).map f = upsert (·.take n) (f x) (l.map f) := by
  induction l with
  | nil => rfl
  | cons y ys ih =>
    by_cases hk : key y = key x
    · simp [upsert, hk, (h y).2 hk]
    · have : ¬ (f y).take n = (f x).take n := fun e => hk ((h y).1 e)
      simp [upsert, hk, this, ih]

theorem map_filter {α β : Type} {_ : DecidableEq (List β)} (q : α → Bool) (f : α → List β)
    (n : Nat) (k : List β) (l : List α) (h : ∀ y, q y = !decide ((f y).take n = k)) :
    (l.filter q).map f = (l.map f).filter (fun r => ¬ r.take n = k) := by
  rw [List.filter_map]
  congr 1
  apply List.filter_congr
  intro y _
  simp [h y]

theorem encode_step (s : St) (w : Write) :
    enc (applyWrite s w) = match sqlA w with
      | some a => applyW kl (enc s) a
      | none => enc s := by
  funext t
  cases w with
  | PutProject p =>
    simp only [sqlA, applyW, applyWrite]
    split
    · subst_vars
      exact map_upsert _ _ 1 _ _ (fun y => by simp [Project.row, int_u64])
    · rename_i ht; rcases t with _ | _ | _ | _ | _ | t <;> simp_all [enc]
  | PutMember m =>
    simp only [sqlA, applyW, applyWrite]
    split
    · subst_vars
      exact map_upsert _ _ 2 _ _ (fun y => by simp [Member.row, int_u64])
    · rename_i ht; rcases t with _ | _ | _ | _ | _ | t <;> simp_all [enc]
  | DelMember p u =>
    simp only [sqlA, applyW, applyWrite]
    split
    · subst_vars
      exact map_filter _ _ 2 _ _ (fun y => by simp [Member.row, int_u64])
    · rename_i ht; rcases t with _ | _ | _ | _ | _ | t <;> simp_all [enc]
  | PutDocument d =>
    simp only [sqlA, applyW, applyWrite]
    split
    · subst_vars
      exact map_upsert _ _ 1 _ _ (fun y => by simp [Document.row, int_u64])
    · rename_i ht; rcases t with _ | _ | _ | _ | _ | t <;> simp_all [enc]
  | DelDocument i =>
    simp only [sqlA, applyW, applyWrite]
    split
    · subst_vars
      exact map_filter _ _ 1 _ _ (fun y => by simp [Document.row, int_u64])
    · rename_i ht; rcases t with _ | _ | _ | _ | _ | t <;> simp_all [enc]
  | SetCounter c =>
    simp only [sqlA, applyW, applyWrite]
    split
    · subst_vars; simp [enc, upsert, kl]
    · rename_i ht; rcases t with _ | _ | _ | _ | _ | t <;> simp_all [enc]
  | PutWebhook h =>
    simp only [sqlA, applyW, applyWrite]
    split
    · subst_vars
      exact map_upsert _ _ 1 _ _ (fun y => by simp [Webhook.row, int_u64])
    · rename_i ht; rcases t with _ | _ | _ | _ | _ | t <;> simp_all [enc]
  | DelWebhook p =>
    simp only [sqlA, applyW, applyWrite]
    split
    · subst_vars
      exact map_filter _ _ 1 _ _ (fun y => by simp [Webhook.row, int_u64])
    · rename_i ht; rcases t with _ | _ | _ | _ | _ | t <;> simp_all [enc]
  | Emit e => simp [sqlA, applyWrite]

/-- The table writes of a write set turn the rows of `s` into the rows of
`applyAll s ws`. No invariant needed. -/
theorem encode_applyAll (s : St) (ws : List Write) :
    enc (applyAll s ws) = applyAllW kl (enc s) (ws.filterMap sqlA) := by
  induction ws generalizing s with
  | nil => rfl
  | cons w ws ih =>
    simp only [applyAll, List.foldl_cons] at ih ⊢
    rw [ih, List.filterMap_cons]
    have := encode_step s w
    split <;> rename_i h <;> simp only [h] at this <;> simp [applyAllW, this]

/-! ## Valid states are well keyed -/

theorem nodup_keys {α κ : Type} (l : List α) (key : α → List Val) (id : α → κ)
    (hid : ∀ x y, key x = key y → id x = id y) (h : (l.map id).Nodup) : (l.map key).Nodup := by
  induction l with
  | nil => simp
  | cons x xs ih =>
    simp only [List.map_cons, List.nodup_cons, List.mem_map] at h ⊢
    exact ⟨fun ⟨y, hy, e⟩ => h.1 ⟨y, hy, hid _ _ e⟩, ih h.2⟩

theorem wellKeyed (s : St) (h : Inv s) : WellKeyed kl (enc s) := by
  intro t
  rcases t with _ | _ | _ | _ | _ | t
  · refine ⟨?_, by simp [enc, kl, Project.row]⟩
    simp only [enc, kl, List.map_map]
    exact nodup_keys _ _ (·.id) (fun x y e => by simpa [Project.row, int_u64] using e) h.proj_keys
  · refine ⟨?_, by simp [enc, kl, Member.row]⟩
    simp only [enc, kl, List.map_map]
    exact nodup_keys _ _ (fun m => (m.project, m.user))
      (fun x y e => by simp [Member.row, int_u64] at e; simp [e]) h.member_keys
  · refine ⟨?_, by simp [enc, kl, Document.row]⟩
    simp only [enc, kl, List.map_map]
    exact nodup_keys _ _ (·.id) (fun x y e => by simpa [Document.row, int_u64] using e) h.doc_keys
  · simp [enc, kl, counterRow]
  · refine ⟨?_, by simp [enc, kl, Webhook.row]⟩
    simp only [enc, kl, List.map_map]
    exact nodup_keys _ _ (·.project) (fun x y e => by simpa [Webhook.row, int_u64] using e) h.hook_keys
  · simp [enc]

theorem writeOk (w : Write) (a : AWrite Val) (h : sqlA w = some a) : WriteOk kl a := by
  cases w <;> simp only [sqlA, Option.some.injEq, reduceCtorEq] at h
  all_goals (subst h; simp [WriteOk, kl, Project.row, Member.row, Document.row, counterRow, Webhook.row])

/-- Storing a write set: if the tenant's rows hold a valid state `s`, running
the planned statements leaves exactly the rows of `applyAll s ws`. -/
theorem stored (s : St) (ws : List Write) (h : Inv s) :
    execAll (readBack kl (enc s)) ((ws.filterMap sqlA).map planA) = readBack kl (enc (applyAll s ws)) := by
  rw [encode_applyAll]
  refine (plan_sound kl _ _ (wellKeyed s h) ?_).1.symm
  intro a ha
  obtain ⟨w, _, hw⟩ := List.mem_filterMap.1 ha
  exact writeOk w a hw


/-! ## The extracted encoder computes `sqlA` -/

def Write.abs : h5i_app_sql.Write → AWrite Val
  | .Put t n row => .put t.val n.val row.val
  | .Del t k => .del t.val k.val
  | .DelWhere t c v => .delWhere t.val c.val v

@[step] theorem key1_spec (a : U64) : key1 a ⦃ v => v.val = [int a.val] ⦄ := by
  unfold key1; step* <;> simp_all
@[step] theorem key2_spec (a b : U64) : key2 a b ⦃ v => v.val = [int a.val, int b.val] ⦄ := by
  unfold key2; step* <;> simp_all

@[step]
theorem sql_write_spec (w : Write) : sql_write w ⦃ o => o.map Write.abs = sqlA w ⦄ := by
  unfold sql_write put
  simp only [Project.TABLE, Project.KEY_LEN, Member.TABLE, Member.KEY_LEN, Document.TABLE,
    Document.KEY_LEN, Counter.TABLE, Counter.KEY_LEN, Webhook.TABLE, Webhook.KEY_LEN]
  cases w <;> simp only <;> step* <;> simp_all [Write.abs, sqlA]


theorem filterMap_fold (l : List Write) (acc : List (AWrite Val)) :
    l.foldl (fun acc w => match sqlA w with | some a => acc ++ [a] | none => acc) acc =
      acc ++ l.filterMap sqlA := by
  induction l generalizing acc with
  | nil => simp
  | cons w ws ih => cases h : sqlA w <;> simp [h, ih, List.filterMap_cons]

@[step]
theorem sql_writes_spec (ws : alloc.vec.Vec Write) :
    sql_writes ws ⦃ v => v.val.map Write.abs = ws.val.filterMap sqlA ⦄ := by
  unfold sql_writes sql_writes_loop
  apply WP.spec_mono (loop_fold ws.val (fun v : alloc.vec.Vec h5i_app_sql.Write => v.val.map Write.abs)
    (fun acc w => match sqlA w with | some a => acc ++ [a] | none => acc)
    (fun v k => v.length ≤ k) (fun x => sql_writes_loop.body ws x.1 x.2) ?_ _ 0#usize (by simp) (by simp))
  · intro r hr; rw [hr, filterMap_fold]; simp
  · intro o j hj ho; have := ws.len_ineq; unfold sql_writes_loop.body; h5i_step
    · refine ⟨⟨by scalar_tac, ?_⟩, by scalar_tac⟩
      rw [← o_post]
    · refine ⟨by scalar_tac, ?_⟩
      rw [← o_post]

/-- `stored` on the extracted encoder. Assumes a counter row exists;
`Load.sql_writes_storedC` also covers a fresh tenant. -/
theorem sql_writes_stored (s : St) (h : Inv s) (ws : alloc.vec.Vec Write) :
    sql_writes ws ⦃ v =>
      execAll (readBack kl (enc s)) ((v.val.map Write.abs).map planA) =
        readBack kl (enc (applyAll s ws.val)) ⦄ := by
  apply WP.spec_mono (sql_writes_spec ws)
  intro v hv
  rw [hv]
  exact stored s ws.val h

end docs_kernel.Storage
