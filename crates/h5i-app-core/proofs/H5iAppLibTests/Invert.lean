import H5iAppLib
/-! `h5i_invert` on code shaped like what Aeneas extracts. -/
open Aeneas Aeneas.Std Result H5iAppLib

namespace H5iAppLibTests

inductive Role where
  | Owner | Viewer
deriving DecidableEq

def check (limit : Usize) (i : Usize) : Result Usize := do
  let j ← i + 1#usize
  if j > limit then fail .panic
  else ok j

/-- Names come from the code: `j`, `hj`, and the condition `hc`. -/
example (limit i r : Usize) (h : check limit i = ok r) : r.val = i.val + 1 ∧ r.val ≤ limit.val := by
  unfold check at h
  h5i_invert h
  -- `h : j = r` is substituted: `hj : i + 1#usize = ok r`.
  refine ⟨add_ok_val hj, ?_⟩
  simp only [gt_iff_lt, not_lt] at hc; scalar_tac

def role_ok (r : Role) : Result Bool :=
  match r with
  | .Owner => ok true
  | .Viewer => ok false

/-- `with` names the condition. -/
example (limit i r : Usize) (h : check limit i = ok r) : r.val ≤ limit.val := by
  unfold check at h
  h5i_invert h with hle
  simp only [gt_iff_lt, not_lt] at hle; scalar_tac

def need (b : Bool) : Result (core.result.Result Unit U8) :=
  if b then ok (.Ok ()) else ok (.Err 1#u8)

/-- `?`: `let r ← need b; let cf ← branch r; match cf with ...`. -/
def act (b : Bool) (x : Usize) : Result (core.result.Result Usize U8) := do
  let r ← need b
  let cf ← core.result.Result.Insts.CoreOpsTry.branch r
  match cf with
  | core.ops.control_flow.ControlFlow.Continue _ =>
    let y ← x + 1#usize
    ok (core.result.Result.Ok y)
  | core.ops.control_flow.ControlFlow.Break residual =>
    core.result.Result.Insts.CoreOpsTry_traitFromResidualResult.from_residual Usize
      (core.convert.FromSame U8) residual

example (b : Bool) (x y : Usize) (h : act b x = ok (.Ok y)) : b = true ∧ y.val = x.val + 1 := by
  unfold act at h
  h5i_invert h
  -- The `Ok` path only: `Err` returns `Err`, not `Ok y`.
  simp only [need] at hr
  h5i_invert hr
  exact ⟨hc, add_ok_val hy_1⟩

end H5iAppLibTests

namespace H5iAppLibTests
open H5iAppLib

/-- `h5i_arith` reads the arithmetic equations a path leaves. -/
example (i j k n : Usize) (h1 : i + 1#usize = ok j) (h2 : j - 1#usize = ok k) (hn : n = core.num.Usize.saturating_sub i 5#usize) :
    k = i ∧ n.val ≤ i.val := by
  constructor
  · apply UScalar.eq_of_val_eq; h5i_arith
  · h5i_arith

example (v : alloc.vec.Vec U8) (i : Usize) (x : U8) (h : v.index_usize i = ok x) : x ∈ v.val ∧ i.val < v.length := by
  h5i_ok_facts
  exact ⟨h_val_1, by simpa using (List.getElem?_eq_some_iff.1 h_val).1⟩

end H5iAppLibTests
