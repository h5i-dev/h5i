import H5iAppSql
import H5iAppLib.Sql
import H5iAppLib.Basic
/-!
# The extracted planner, and what its statements mean

`plan` turns a write set into keyed statements. We prove it computes
`planA` on plain lists, then give statements a database semantics and show
that running the plan agrees with applying the writes to keyed tables.
-/
open Aeneas Aeneas.Std Result h5i_app_sql H5iAppLib H5iAppLib.Sql

namespace h5i_app_sql.Sem

/-! ## Plain-list views of the extracted types -/

def Stmt.abs : Stmt → AStmt Val
  | .Upsert t k r => .up t.val k.val r.val
  | .Delete t k => .del t.val k.val
  | .DeleteWhere t c v => .delWhere t.val c.val v

def Write.abs : Write → AWrite Val
  | .Put t kl row => .put t.val kl.val row.val
  | .Del t k => .del t.val k.val
  | .DelWhere t c v => .delWhere t.val c.val v

/-! ## The extracted planner computes `planA` -/

theorem val_clone (x : Val) : Val.Insts.CoreCloneClone.clone x = ok x := by
  cases x <;> simp [Val.Insts.CoreCloneClone.clone, u8vec_clone, lift]

@[step]
theorem val_clone_spec (x : Val) : Val.Insts.CoreCloneClone.clone x ⦃ y => y = x ⦄ := by
  simp [val_clone]

theorem vals_clone (v : alloc.vec.Vec Val) :
    alloc.vec.CloneVec.clone Val.Insts.CoreCloneClone v = ok v := by
  have h := Slice.clone_spec (clone := Val.Insts.CoreCloneClone.clone) (s := v.slice)
    (fun x _ => val_clone x)
  rw [WP.spec_equiv_exists] at h
  obtain ⟨s', hs, rfl⟩ := h
  simp [alloc.vec.CloneVec.clone, hs]

@[step]
theorem prefix_loop_spec (row : alloc.vec.Vec Val) (n : Usize) :
    prefix_loop row n (alloc.vec.Vec.new Val) 0#usize ⦃ v => v.val = row.val.take n.val ⦄ := by
  unfold prefix_loop
  apply loop.spec_decr_nat
    (measure := fun (x : alloc.vec.Vec Val × Usize) => row.length - x.2.val)
    (inv := fun x => x.2.val ≤ row.length ∧ x.2.val ≤ n.val ∧ x.1.val = row.val.take x.2.val)
  · rintro ⟨o, j⟩ ⟨hj, hn, heq⟩
    simp only at hj hn heq ⊢
    unfold prefix_loop.body
    step*
    · refine ⟨by scalar_tac, by scalar_tac, ?_, by scalar_tac⟩
      rw [out1_post, heq, i2_post, v1_post, v_post, List.take_add_one,
        List.getElem?_eq_getElem (by scalar_tac)]
      rfl
    · rw [heq, List.take_of_length_le (by scalar_tac), List.take_of_length_le (by scalar_tac)]
  · simp

@[step]
theorem suffix_loop_spec (row : alloc.vec.Vec Val) (n : Usize) :
    suffix_loop row (alloc.vec.Vec.new Val) n ⦃ v => v.val = row.val.drop n.val ⦄ := by
  unfold suffix_loop
  apply loop.spec_decr_nat
    (measure := fun (x : alloc.vec.Vec Val × Usize) => row.length - x.2.val)
    (inv := fun x => x.1.length ≤ x.2.val ∧ x.1.val ++ row.val.drop x.2.val = row.val.drop n.val)
  · rintro ⟨o, j⟩ ⟨hl, heq⟩
    simp only at hl heq ⊢
    unfold suffix_loop.body
    step*
    · have hc : row.val.drop j.val = row.val[j.val] :: row.val.drop (j.val + 1) :=
        List.drop_eq_getElem_cons (by scalar_tac)
      refine ⟨?_, ?_, by scalar_tac⟩
      · simp only [alloc.vec.Vec.length, out1_post, List.length_append, List.length_singleton, i2_post]
        simp only [alloc.vec.Vec.length] at hl
        omega
      · rw [← heq, out1_post, i2_post, hc, v1_post, v_post]; simp
    · rw [← heq, List.drop_eq_nil_of_le (by scalar_tac), List.append_nil]
  · simp

@[step]
theorem plan_one_spec (w : Write) : plan_one w ⦃ s => Stmt.abs s = planA (Write.abs w) ⦄ := by
  unfold plan_one
  cases w <;> simp only
  · step*
    simp only [Stmt.abs, Write.abs, planA, v_post, v1_post, n_post, U32.cast_Usize_val_eq]
  · simp [vals_clone, Stmt.abs, Write.abs, planA]
  · simp [val_clone, Stmt.abs, Write.abs, planA]

/-- `plan` succeeds and computes `planA` on every write, in order. -/
@[step]
theorem plan_spec (ws : alloc.vec.Vec Write) :
    plan ws ⦃ ss => ss.val.map Stmt.abs = ws.val.map (planA ∘ Write.abs) ⦄ := by
  unfold plan plan_loop
  apply loop.spec_decr_nat
    (measure := fun (x : alloc.vec.Vec Stmt × Usize) => ws.length - x.2.val)
    (inv := fun x => x.2.val ≤ ws.length ∧ x.1.length = x.2.val ∧
      x.1.val.map Stmt.abs = (ws.val.take x.2.val).map (planA ∘ Write.abs))
  · rintro ⟨o, j⟩ ⟨hj, hl, heq⟩
    simp only at hj hl heq ⊢
    unfold plan_loop.body
    step*
    · refine ⟨by scalar_tac, ?_, ?_, by scalar_tac⟩
      · simp only [alloc.vec.Vec.length, out1_post, List.length_append, List.length_singleton, i2_post]
        simp only [alloc.vec.Vec.length] at hl
        omega
      · have hj' : j.val < ws.val.length := by scalar_tac
        rw [out1_post, i2_post, List.map_append, heq, List.take_add_one,
          List.getElem?_eq_getElem hj', Option.toList_some, List.map_append]
        simp [s_post, w_post]
    · rw [heq, List.take_of_length_le (by scalar_tac)]
  · simp

/-! ## What statements mean: `H5iAppLib.Sql` -/

/-- End to end: the extracted `plan` on an extracted write set, run under the
statement semantics, gives the keyed-table result. -/
theorem extracted_plan_sound (kl : Nat → Nat) (ws : alloc.vec.Vec Write) (tabs : Tables Val)
    (hk : WellKeyed kl tabs) (hw : ∀ w ∈ ws.val, WriteOk kl (Write.abs w)) :
    plan ws ⦃ ss =>
      execAll (readBack kl tabs) (ss.val.map Stmt.abs) =
        readBack kl (applyAllW kl tabs (ws.val.map Write.abs)) ⦄ := by
  have h := plan_spec ws
  refine WP.spec_mono h ?_
  intro ss hss
  rw [hss, ← List.map_map]
  exact (plan_sound kl _ tabs hk (by simpa using hw)).1.symm

/-- From an empty database, running every batch's plan in order gives the
keyed-table result of all the writes. Discharges `WellKeyed`. -/
theorem history_sound (kl : Nat → Nat) (bs : List (List (AWrite Val)))
    (hw : ∀ b ∈ bs, ∀ w ∈ b, WriteOk kl w) :
    execAll (fun _ _ => none) (bs.flatMap (·.map planA)) =
      readBack kl (applyAllW kl (fun _ => []) bs.flatten) := by
  have hk : WellKeyed kl (V := Val) (fun _ => []) := fun _ => by simp
  have h := (plan_sound kl bs.flatten (fun _ => []) hk
    (fun w hw' => by
      obtain ⟨b, hb, hwb⟩ := List.mem_flatten.1 hw'
      exact hw b hb w hwb)).1
  have e0 : readBack kl (V := Val) (fun _ => []) = fun _ _ => none := by
    funext t k; simp [readBack]
  rw [h, e0, List.map_flatten]
  rfl

/-- Non-vacuity: a put then a delete of another key leaves the put's row. -/
example : execAll (fun _ _ => none)
    ([AWrite.put 0 1 [Val.Int 1#i64, Val.Bool true], AWrite.del 0 [Val.Int 2#i64]].map planA)
      0 [Val.Int 1#i64] = some [Val.Int 1#i64, Val.Bool true] := by
  simp [execAll, exec, planA]

end h5i_app_sql.Sem
