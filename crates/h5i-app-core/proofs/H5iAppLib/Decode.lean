import H5iAppLib.Loops
/-! `rows_loop`: the `from_rows` loop `schema!` generates decodes rows that
encode a list to that list. -/
open Aeneas Aeneas.Std Result

namespace H5iAppLib

/-- The body of every generated `from_rows` loop, for its row decoder `F`. -/
def rowsBody {T V : Type} (F : alloc.vec.Vec V → Result (Option T))
    (rows : alloc.vec.Vec (alloc.vec.Vec V)) (out : alloc.vec.Vec T) (ok1 : Bool) (i : Usize) :
    Result (ControlFlow ((alloc.vec.Vec T) × Bool × Usize) ((alloc.vec.Vec T) × Bool)) := do
  let i1 := alloc.vec.Vec.len rows
  if i < i1
  then
    let v ← alloc.vec.Vec.index (core.slice.index.SliceIndexUsizeSlice (alloc.vec.Vec V)) rows i
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

theorem rows_loop {T V : Type} (F : alloc.vec.Vec V → Result (Option T)) (f : T → List V)
    (hF : ∀ x v, v.val = f x → F v ⦃ o => o = some x ⦄)
    (rows : alloc.vec.Vec (alloc.vec.Vec V)) (l : List T) (h : rows.val.map (·.val) = l.map f) :
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

end H5iAppLib
