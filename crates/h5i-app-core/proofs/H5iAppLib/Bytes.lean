import H5iAppLib.Basic
/-!
# Byte strings

Aeneas has no model of `String`, so h5i-app kernels keep text as `Vec<u8>` or
`&[u8]`. These specs cover what text code extracts to besides loops: `==` on
byte slices and byte-string literals (`b"owner".as_slice()`).
-/
open Aeneas Aeneas.Std Result

namespace H5iAppLib

/-- `==` on byte slices. -/
@[step] theorem slice_u8_eq_spec (s t : Slice U8) :
    core.slice.cmp.PartialEqSlice.eq core.cmp.PartialEqU8 s t ⦃ b => b = decide (s = t) ⦄ := by
  apply WP.spec_mono (core.slice.cmp.PartialEqSlice.eq_homo_spec core.cmp.PartialEqU8 s t (fun x y => by
    simp only [WP.spec_ok]
    by_cases h : x = y <;> simp [h]))
  intro b hb
  cases b <;> simp_all

theorem anyM_u8_ne (l : List (U8 × U8)) :
    List.anyM (fun (p : U8 × U8) => core.cmp.PartialEqU8.ne p.1 p.2) l =
      ok (l.any (fun p => !decide (p.1 = p.2))) := by
  induction l with
  | nil => rfl
  | cons p ps ih =>
    by_cases h : p.1 = p.2
    · simp_all [List.anyM, core.cmp.impls.PartialEqU8.ne]
    · simp [List.anyM, h, core.cmp.impls.PartialEqU8.ne]; rfl

/-- `!=` on byte strings. -/
@[step] theorem vec_u8_ne_spec (v w : alloc.vec.Vec U8) :
    alloc.vec.partial_eq.PartialEqVec.ne core.cmp.PartialEqU8 v w ⦃ b => b = !decide (v = w) ⦄ := by
  unfold alloc.vec.partial_eq.PartialEqVec.ne
  split
  · rename_i hlen
    rw [show (fun (x : U8 × U8) => match x with | (x0, x1) => core.cmp.PartialEqU8.ne x0 x1) =
        (fun p => core.cmp.PartialEqU8.ne p.1 p.2) from rfl, anyM_u8_ne]
    simp only [WP.spec_ok]
    have hz := zip_all_eq v.val w.val (by simpa using hlen)
    have hd : decide (v = w) = decide (v.val = w.val) := decide_eq_decide.2 (alloc.vec.Vec.eq_iff v w)
    rw [hd, ← hz, List.all_eq_not_any_not, Bool.not_not]
  · rename_i hlen
    simp only [WP.spec_ok]
    have : v ≠ w := fun h => hlen (by simp [h])
    simp [this]

/-- A byte-string literal, as Aeneas extracts it. -/
theorem array_to_slice_val {α} {n : Usize} (a : Array α n) : (Array.to_slice a).val = a.val := by
  simp [Array.to_slice]

end H5iAppLib
