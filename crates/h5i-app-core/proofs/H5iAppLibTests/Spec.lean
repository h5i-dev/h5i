import H5iAppLib
/-! `@[h5i_spec]` derives the equation forms of a spec. -/
open Aeneas Aeneas.Std Result H5iAppLib

namespace H5iAppLibTests.Spec

def double (x : U32) : Result U32 := x + x

@[h5i_spec] theorem double_spec (x : U32) (h : 2 * x.val ≤ U32.max) : double x ⦃ y => y.val = 2 * x.val ⦄ := by
  unfold double; step*

example (x y : U32) (h : double x = ok y) (hb : 2 * x.val ≤ U32.max) : y.val = 2 * x.val :=
  double_spec.inv x hb y h

def one : Result U32 := ok 1#u32

@[h5i_spec] theorem one_spec : one ⦃ r => r = 1#u32 ⦄ := by simp [one]

example : one = ok 1#u32 := one_spec.eq

end H5iAppLibTests.Spec
