import H5iAppLib.Basic
/-!
# From `= ok` back to values

`h5i_invert` leaves the equations of the calls on a successful path:
`h : i + 1#usize = ok j`, `h : v.index_usize i = ok x`, `cf ← branch r`.
These lemmas turn each into what it says about values, and `h5i_arith`
rewrites every arithmetic one at once before `scalar_tac`.

The `Result` monad is an interaction tree, so `rfl`, `cases h` and
`injection h` on these equations often fail; `ok_inj`, `ok_ne_fail` and the
lemmas below are the way through.
-/
open Aeneas Aeneas.Std Result

namespace H5iAppLib

/-! ## `Result` -/

theorem result_ok_inj {α} {a b : α} (h : (ok a : Result α) = ok b) : a = b := by
  simpa using h

theorem ok_ne_fail {α} {a : α} {e : Error} : (ok a : Result α) ≠ fail e := by simp

theorem fail_ne_ok {α} {a : α} {e : Error} : (fail e : Result α) ≠ ok a := by simp

/-- For `simp`: a path that fails does not return `ok`. -/
theorem fail_eq_ok {α} {a : α} {e : Error} : ((fail e : Result α) = ok a) = False := by simp

/-- `ok` is injective, under the name a constructor's would have. With
`open H5iAppLib`, `Result.ok.inj h`. (`WP.spec_ok` is `spec_ok` once
`Aeneas.Std.WP` is open, as `h5i app new` writes it.) -/
theorem Result.ok.inj {α} {a b : α} (h : (ok a : Aeneas.Std.Result α) = ok b) : a = b := result_ok_inj h

/-! ## Scalars -/

variable {ty : UScalarTy}

theorem add_ok_val {x y z : UScalar ty} (h : x + y = ok z) : z.val = x.val + y.val := by
  have := UScalar.add_equiv x y; rw [h] at this; simp_all

theorem add_ok_bound {x y z : UScalar ty} (h : x + y = ok z) : x.val + y.val ≤ UScalar.max ty := by
  have := UScalar.add_equiv x y; rw [h] at this; simp at this
  rw [UScalar.max_def]; omega

theorem sub_ok_val {x y z : UScalar ty} (h : x - y = ok z) : z.val = x.val - y.val ∧ y.val ≤ x.val := by
  have := UScalar.sub_equiv x y; rw [h] at this; simp at this; omega

theorem mul_ok_val {x y z : UScalar ty} (h : x * y = ok z) : z.val = x.val * y.val := by
  have := UScalar.mul_equiv x y; rw [show x.mul y = x * y from rfl, h] at this; simp_all

theorem div_ok_val {x y z : UScalar ty} (h : x / y = ok z) : z.val = x.val / y.val ∧ y.val ≠ 0 := by
  by_cases hy : y.val = 0
  · have : y.bv = 0 := by rw [← UScalar.bv_toNat] at hy; exact BitVec.eq_of_toNat_eq (by simpa using hy)
    simp [HDiv.hDiv, UScalar.div, this] at h
  · obtain ⟨w, hw, hv⟩ := UScalar.div_spec x hy
    rw [hw] at h; cases result_ok_inj h; exact ⟨hv, hy⟩

theorem rem_ok_val {x y z : UScalar ty} (h : x % y = ok z) : z.val = x.val % y.val ∧ y.val ≠ 0 := by
  by_cases hy : y.val = 0
  · have : y.bv = 0 := by rw [← UScalar.bv_toNat] at hy; exact BitVec.eq_of_toNat_eq (by simpa using hy)
    simp [HMod.hMod, UScalar.rem, hy] at h
  · exact ⟨post_of_ok (P := fun z : UScalar ty => z.val = x.val % y.val) (UScalar.rem_spec x hy) h, hy⟩

theorem saturating_sub_val (x y : UScalar ty) : (UScalar.saturating_sub x y).val = x.val - y.val := by
  simp only [UScalar.saturating_sub, UScalar.val, BitVec.toNat_ofNat]
  apply Nat.mod_eq_of_lt
  have := x.bv.isLt; simp only [Nat.zero_max]; omega

theorem saturating_add_val (x y : UScalar ty) :
    (UScalar.saturating_add x y).val = min (x.val + y.val) (UScalar.max ty) := by
  simp only [UScalar.saturating_add, UScalar.val, BitVec.toNat_ofNat]
  rw [Nat.min_comm]; apply Nat.mod_eq_of_lt
  have : UScalar.max ty < 2 ^ ty.numBits := by rw [UScalar.max_def]; have := Nat.two_pow_pos ty.numBits; omega
  omega

/-- A widening cast keeps the value. -/
theorem cast_val {src : UScalarTy} (tgt : UScalarTy) (x : UScalar src) (h : x.val ≤ UScalar.max tgt) :
    (UScalar.cast tgt x).val = x.val := by
  rw [UScalar.cast_val_eq]; apply Nat.mod_eq_of_lt
  rw [UScalar.max_def] at h; have := Nat.two_pow_pos tgt.numBits; omega

/-! ## Indexing -/

theorem vec_index_ok {α} {v : alloc.vec.Vec α} {i : Usize} {x : α} (h : v.index_usize i = ok x) :
    ∃ hi : i.val < v.val.length, v.val[i.val] = x := by
  simp only [alloc.vec.Vec.index_usize] at h
  split at h
  · simp at h
  · rename_i y hy; simp at h; subst h
    simp only [alloc.vec.Vec.getElem?_Nat_eq, List.getElem?_eq_some_iff] at hy ⊢
    exact hy

theorem vec_index_ok_get? {α} {v : alloc.vec.Vec α} {i : Usize} {x : α} (h : v.index_usize i = ok x) :
    v.val[i.val]? = some x := by
  obtain ⟨hi, rfl⟩ := vec_index_ok h; simp

theorem vec_index_ok_mem {α} {v : alloc.vec.Vec α} {i : Usize} {x : α} (h : v.index_usize i = ok x) :
    x ∈ v.val := by
  obtain ⟨hi, rfl⟩ := vec_index_ok h; simp

/-- `v[i]` extracts as `Vec.index (SliceIndexUsizeSlice _) v i`. -/
theorem vec_index_slice_ok_get? {α} {v : alloc.vec.Vec α} {i : Usize} {x : α}
    (h : alloc.vec.Vec.index (core.slice.index.SliceIndexUsizeSlice α) v i = ok x) : v.val[i.val]? = some x :=
  vec_index_ok_get? (by rwa [alloc.vec.Vec.index_slice_index] at h)

theorem vec_index_slice_ok_mem {α} {v : alloc.vec.Vec α} {i : Usize} {x : α}
    (h : alloc.vec.Vec.index (core.slice.index.SliceIndexUsizeSlice α) v i = ok x) : x ∈ v.val :=
  vec_index_ok_mem (by rwa [alloc.vec.Vec.index_slice_index] at h)

theorem slice_index_ok {α} {s : Slice α} {i : Usize} {x : α} (h : s.index_usize i = ok x) :
    ∃ hi : i.val < s.val.length, s.val[i.val] = x := by
  simp only [Slice.index_usize] at h
  split at h
  · simp at h
  · rename_i y hy; simp at h; subst h
    simp only [Slice.getElem?_Usize_eq, List.getElem?_eq_some_iff] at hy ⊢
    exact hy

theorem slice_index_ok_mem {α} {s : Slice α} {i : Usize} {x : α} (h : s.index_usize i = ok x) :
    x ∈ s.val := by
  obtain ⟨hi, rfl⟩ := slice_index_ok h; simp

/-! ## `?`

`r?` extracts as `let cf ← branch r; match cf with | Continue v => k v |
Break res => from_residual T inst res`. The `match` is the generated file's
own, so no lemma can name it; `bind_branch` instead moves the case split onto
`r`, where `simp` reduces the generated `match` on a constructor. -/

theorem bind_branch {T E β : Type} (r : core.result.Result T E)
    (k : core.ops.control_flow.ControlFlow (core.result.Result Never E) T → Result β) :
    Std.bind (core.result.Result.Insts.CoreOpsTry.branch r) k =
      match r with
      | .Ok v => k (.Continue v)
      | .Err e => k (.Break (.Err e)) := by
  cases r <;> simp [core.result.Result.Insts.CoreOpsTry.branch]

theorem bind_tc_branch {T E β : Type} (r : core.result.Result T E)
    (k : core.ops.control_flow.ControlFlow (core.result.Result Never E) T → Result β) :
    (do let cf ← core.result.Result.Insts.CoreOpsTry.branch r; k cf) =
      match r with
      | .Ok v => k (.Continue v)
      | .Err e => k (.Break (.Err e)) := by
  cases r <;> simp [core.result.Result.Insts.CoreOpsTry.branch]

/-- `?` on the same error type passes the error through. -/
@[simp] theorem from_residual_same {U E : Type} (e : E) :
    core.result.Result.Insts.CoreOpsTry_traitFromResidualResult.from_residual U (core.convert.FromSame E)
      (.Err e) = ok (.Err e) := by
  simp [core.result.Result.Insts.CoreOpsTry_traitFromResidualResult.from_residual, core.convert.FromSame.from]

theorem from_residual_err {U E F : Type} (inst : core.convert.From F E) (e : E) :
    core.result.Result.Insts.CoreOpsTry_traitFromResidualResult.from_residual U inst (.Err e) =
      (do let f ← inst.from e; ok (.Err f)) := rfl

theorem branch_ok_eq {T E : Type} {r : core.result.Result T E} {cf} (h : core.result.Result.Insts.CoreOpsTry.branch r = ok cf) :
    match r with
    | .Ok v => cf = .Continue v
    | .Err e => cf = .Break (.Err e) := by
  cases r <;> simp [core.result.Result.Insts.CoreOpsTry.branch] at h <;> simp [h]

end H5iAppLib
