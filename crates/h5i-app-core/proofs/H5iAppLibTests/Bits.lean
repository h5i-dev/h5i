import H5iAppLib
/-! Prefix masks and `h5i_bv` on code shaped like an extraction. -/
open Aeneas Aeneas.Std Result H5iAppLib

namespace H5iAppLibTests.Bits

/-- `fn contains(net: u32, addr: u32, len: u32) -> bool` with
`let mask = u32::MAX << (32 - len); net & mask == addr & mask`. -/
def contains (net addr len : U32) : Result Bool := do
  let s ← 32#u32 - len
  let mask ← core.num.U32.MAX <<< s
  ok ((net &&& mask) = (addr &&& mask))

theorem contains_spec (net addr len : U32) (h1 : 1 ≤ len.val) (h2 : len.val ≤ 32) :
    contains net addr len ⦃ b => b = decide (net.val / 2 ^ (32 - len.val) = addr.val / 2 ^ (32 - len.val)) ⦄ := by
  unfold contains
  step*
  simp only [u32_masked_eq_iff net addr mask s.val mask_post1, s_post]

example (x y : U32) : (x &&& y) = (y &&& x) := by h5i_bv

example (x : U64) : (x ||| core.num.U64.MAX) = core.num.U64.MAX := by h5i_bv

/-- Hypotheses take part. -/
example (x y z : U32) (h : (x &&& y) = z) : (z &&& x) = z := by h5i_bv

/-- The mask of a prefix only grows when the shift shrinks. -/
example (x m : U32) (s : Nat) (hm : m.bv = core.num.U32.MAX.bv <<< s) : (x &&& m &&& m) = (x &&& m) := by h5i_bv

end H5iAppLibTests.Bits
