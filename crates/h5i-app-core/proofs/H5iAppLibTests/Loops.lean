import H5iAppLib
/-! The loop tactics on loops shaped like what Aeneas extracts. -/
open Aeneas Aeneas.Std Result ControlFlow H5iAppLib

namespace H5iAppLibTests.Loops

def exists_loop.body (v : alloc.vec.Vec U64) (k : U64) (i : Usize) : Result (ControlFlow Usize Bool) := do
  let i1 := alloc.vec.Vec.len v
  if i < i1
  then
    let u ← alloc.vec.Vec.index (core.slice.index.SliceIndexUsizeSlice U64) v i
    if u = k
    then ok (done true)
    else let i2 ← i + 1#usize
         ok (cont i2)
  else ok (done false)

def exists_loop (v : alloc.vec.Vec U64) (k : U64) (i : Usize) : Result Bool :=
  loop (exists_loop.body v k) i

def exists_ (v : alloc.vec.Vec U64) (k : U64) : Result Bool := exists_loop v k 0#usize

theorem exists_spec (v : alloc.vec.Vec U64) (k : U64) : exists_ v k ⦃ b => b = v.val.any (· = k) ⦄ := by
  unfold exists_ exists_loop
  h5i_search_any v.val (fun u => decide (u = k))

def none_loop.body (v : alloc.vec.Vec U64) (k : U64) (i : Usize) : Result (ControlFlow Usize Bool) := do
  let i1 := alloc.vec.Vec.len v
  if i < i1
  then
    let u ← alloc.vec.Vec.index (core.slice.index.SliceIndexUsizeSlice U64) v i
    if u = k
    then ok (done false)
    else let i2 ← i + 1#usize
         ok (cont i2)
  else ok (done true)

theorem none_spec (v : alloc.vec.Vec U64) (k : U64) :
    loop (none_loop.body v k) 0#usize ⦃ b => b = v.val.all (fun u => !decide (u = k)) ⦄ := by
  h5i_search_all v.val (fun u => decide (u = k))

/-- `h5i_total`: the loop does not fail. -/
theorem exists_total (v : alloc.vec.Vec U64) (k : U64) : loop (exists_loop.body v k) 0#usize ⦃ _ => True ⦄ := by
  h5i_total (fun i => i) v.length

/-- `loop_true_witness`, from the equation alone. -/
example (v : alloc.vec.Vec U64) (k : U64) (h : loop (exists_loop.body v k) 0#usize = ok true) : k ∈ v.val := by
  refine loop_true_witness _ (fun _ => True) (fun i => v.length - i.val) _ ?_ _ trivial h
  intro i r _ hr
  unfold exists_loop.body at hr
  h5i_invert hr
  · intro _; subst hc_1; exact vec_index_slice_ok_mem hu
  · exact ⟨trivial, by h5i_arith⟩
  · simp

/-- `h5i_measure_induction`: a function recursing on an index. -/
def countFrom (l : List Nat) (i : Nat) : Nat :=
  if h : i < l.length then (if l[i] = 0 then 1 else 0) + countFrom l (i + 1) else 0
termination_by l.length - i

example (l : List Nat) (i : Nat) : countFrom l i ≤ l.length - i := by
  h5i_measure_induction l.length - i with ih
  unfold countFrom
  split
  · have := ih (l.length - (i + 1)) (by omega) l (i + 1) rfl
    split <;> omega
  · omega

end H5iAppLibTests.Loops
