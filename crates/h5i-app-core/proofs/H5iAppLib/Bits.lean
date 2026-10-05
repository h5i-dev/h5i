import H5iAppLib.Basic
/-!
# Bitmask permissions

A permission set kept as bits of a `u64` (bitflags, Allow/Deny masks) checks
`held.contains(required)`, which is `(held & required) == required`. `Covers`
is that check; the lemmas move it to bits (`covers_iff_testBit`), where
reflexivity, transitivity and unions are one line each. `bv_decide` does not
take `&&&` on Aeneas scalars, and `bvify` needs a width, so this is the path
from the extracted `&&&` to a proof.

The extracted check is `let i ← lift (held &&& required); if i = required ...`:
`step*` takes the `lift` (`UScalar.and_spec`), and `covers_def` names the
condition.
-/
open Aeneas Aeneas.Std Result

namespace H5iAppLib

variable {ty : UScalarTy}

/-- `held` has every bit of `req`: `(held & req) == req`. -/
def Covers (held req : UScalar ty) : Prop := held &&& req = req

instance (held req : UScalar ty) : Decidable (Covers held req) := by unfold Covers; infer_instance

theorem covers_def (held req : UScalar ty) : (held &&& req = req) = Covers held req := rfl

theorem covers_iff_val {held req : UScalar ty} : Covers held req ↔ held.val &&& req.val = req.val := by
  unfold Covers
  constructor
  · intro h; rw [← UScalar.val_and, h]
  · intro h; apply UScalar.eq_of_val_eq; rw [UScalar.val_and, h]

theorem covers_iff_testBit {held req : UScalar ty} :
    Covers held req ↔ ∀ i, req.val.testBit i = true → held.val.testBit i = true := by
  rw [covers_iff_val]
  constructor
  · intro h i hr
    have := congrArg (fun n => n.testBit i) h
    simp only [Nat.testBit_and, hr, Bool.and_true] at this
    exact this
  · intro h
    apply Nat.eq_of_testBit_eq
    intro i
    rw [Nat.testBit_and]
    cases hr : req.val.testBit i
    · simp
    · simp [h i hr]

theorem covers_refl (h : UScalar ty) : Covers h h := by
  rw [covers_iff_testBit]; exact fun _ x => x

/-- Nothing required: every set covers it. -/
theorem covers_of_val_zero (h : UScalar ty) {r : UScalar ty} (hr : r.val = 0) : Covers h r := by
  rw [covers_iff_testBit]; intro i; simp [hr]

theorem Covers.trans {a b c : UScalar ty} (h₁ : Covers a b) (h₂ : Covers b c) : Covers a c := by
  rw [covers_iff_testBit] at *; exact fun i h => h₁ i (h₂ i h)

/-- A union is covered iff both parts are. -/
theorem covers_or_iff {h a b : UScalar ty} : Covers h (a ||| b) ↔ Covers h a ∧ Covers h b := by
  simp only [covers_iff_testBit, UScalar.val_or, Nat.testBit_or, Bool.or_eq_true]
  constructor
  · intro H; exact ⟨fun i hi => H i (Or.inl hi), fun i hi => H i (Or.inr hi)⟩
  · rintro ⟨Ha, Hb⟩ i (hi | hi); exact Ha i hi; exact Hb i hi

/-- Granting more bits keeps what was held. -/
theorem covers_or_left (a b : UScalar ty) : Covers (a ||| b) a := by
  simp only [covers_iff_testBit, UScalar.val_or, Nat.testBit_or, Bool.or_eq_true]
  exact fun _ h => Or.inl h

theorem covers_or_right (a b : UScalar ty) : Covers (a ||| b) b := by
  simp only [covers_iff_testBit, UScalar.val_or, Nat.testBit_or, Bool.or_eq_true]
  exact fun _ h => Or.inr h

/-- Masking (`allow & !deny`, `held & ceiling`) only removes bits. -/
theorem covers_and_left (a b : UScalar ty) : Covers a (a &&& b) := by
  simp only [covers_iff_testBit, UScalar.val_and, Nat.testBit_and, Bool.and_eq_true]
  exact fun _ h => h.1

theorem covers_and_right (a b : UScalar ty) : Covers b (a &&& b) := by
  simp only [covers_iff_testBit, UScalar.val_and, Nat.testBit_and, Bool.and_eq_true]
  exact fun _ h => h.2

/-- What a mask keeps is covered by the mask: deny-wins and ceilings. -/
theorem Covers.of_and {h m r : UScalar ty} (hc : Covers (h &&& m) r) : Covers m r :=
  (covers_and_right h m).trans hc

/-! ## Prefix masks (CIDR)

`addr & (u32::MAX << (32 - prefix))` keeps the top bits of an address; two
addresses agree under the mask iff they agree after dropping the low `k`
bits. `step*` on `MAX <<< s` gives `m.bv = MAX.bv <<< s.val`; `max_bv`
names `MAX.bv`. -/

/-- The bit-level fact: masking off the low `k` bits equates exactly what
shifting them out does. -/
theorem bv_masked_eq_iff {n : Nat} (x y : BitVec n) (k : Nat) :
    (x &&& (BitVec.allOnes n <<< k)) = (y &&& (BitVec.allOnes n <<< k)) ↔ x >>> k = y >>> k := by
  constructor
  · intro h
    apply BitVec.eq_of_getLsbD_eq
    intro i hi
    have := congrArg (·.getLsbD (i + k)) h
    simp only [BitVec.getLsbD_and, BitVec.getLsbD_shiftLeft, BitVec.getLsbD_allOnes, BitVec.getLsbD_ushiftRight] at this ⊢
    by_cases hik : i + k < n
    · simp [hik, show ¬ (i + k < k) by omega, show i < n by omega] at this
      rw [Nat.add_comm, BitVec.getLsbD_eq_getElem hik, BitVec.getLsbD_eq_getElem hik]; exact this
    · rw [BitVec.getLsbD_of_ge x (k + i) (by omega), BitVec.getLsbD_of_ge y (k + i) (by omega)]
  · intro h
    apply BitVec.eq_of_getLsbD_eq
    intro i hi
    simp only [BitVec.getLsbD_and, BitVec.getLsbD_shiftLeft, BitVec.getLsbD_allOnes]
    by_cases hik : i < k
    · simp [hik]
    · have := congrArg (·.getLsbD (i - k)) h
      simp only [BitVec.getLsbD_ushiftRight] at this
      rw [show k + (i - k) = i by omega] at this
      simp only [hik, decide_false, Bool.not_false, Bool.and_true, show i - k < n by omega, decide_true]
      simpa [BitVec.getLsbD_eq_getElem hi, hi] using this

theorem masked_eq_iff (x y m : UScalar ty) (k : Nat) (hm : m.bv = BitVec.allOnes _ <<< k) :
    (x &&& m) = (y &&& m) ↔ x.val / 2 ^ k = y.val / 2 ^ k := by
  rw [UScalar.eq_equiv_bv_eq, UScalar.bv_and, UScalar.bv_and, hm, bv_masked_eq_iff, ← BitVec.toNat_inj]
  simp [BitVec.toNat_ushiftRight, Nat.shiftRight_eq_div_pow]

theorem u8_max_bv : core.num.U8.MAX.bv = BitVec.allOnes UScalarTy.U8.numBits := by simp [core.num.U8.MAX]; rfl
theorem u16_max_bv : core.num.U16.MAX.bv = BitVec.allOnes UScalarTy.U16.numBits := by simp [core.num.U16.MAX]; rfl
theorem u32_max_bv : core.num.U32.MAX.bv = BitVec.allOnes UScalarTy.U32.numBits := by simp [core.num.U32.MAX]; rfl
theorem u64_max_bv : core.num.U64.MAX.bv = BitVec.allOnes UScalarTy.U64.numBits := by simp [core.num.U64.MAX]; rfl
theorem u128_max_bv : core.num.U128.MAX.bv = BitVec.allOnes UScalarTy.U128.numBits := by simp [core.num.U128.MAX]; rfl

/-- The extracted shape: `let m ← core.num.U32.MAX <<< s`, then
`x &&& m = y &&& m`. -/
theorem u32_masked_eq_iff (x y m : U32) (s : Nat) (hm : m.bv = core.num.U32.MAX.bv <<< s) :
    (x &&& m) = (y &&& m) ↔ x.val / 2 ^ s = y.val / 2 ^ s :=
  masked_eq_iff (ty := .U32) x y m s (by simp only [u32_max_bv] at hm; exact hm)

theorem u64_masked_eq_iff (x y m : U64) (s : Nat) (hm : m.bv = core.num.U64.MAX.bv <<< s) :
    (x &&& m) = (y &&& m) ↔ x.val / 2 ^ s = y.val / 2 ^ s :=
  masked_eq_iff (ty := .U64) x y m s (by simp only [u64_max_bv] at hm; exact hm)

open Lean Meta in
/-- The width of the first unsigned scalar variable in the context. -/
private def scalarWidth? : MetaM (Option Nat) := do
  for d in ← getLCtx do
    if d.isImplementationDetail then continue
    let ty ← whnfR d.type
    if ty.isAppOfArity ``UScalar 1 then
      match ty.appArg!.constName? with
      | some ``UScalarTy.U8 => return some 8
      | some ``UScalarTy.U16 => return some 16
      | some ``UScalarTy.U32 => return some 32
      | some ``UScalarTy.U64 => return some 64
      | some ``UScalarTy.U128 => return some 128
      | _ => pure ()
  return none

open Lean Elab Tactic Meta in
/-- `h5i_bv`: a goal about `&&&`, `|||`, `^^^`, `~~~`, shifts, `MAX` and
equality of unsigned scalars, by `bv_decide`. `bv_decide` rejects `UScalar`
as it is; this runs Aeneas's `bvify` at the width of the scalars in the
context and spells out `MAX`. `h5i_bv n` picks the width. -/
elab "h5i_bv" n:(ppSpace num)? : tactic => withMainContext do
  let w ← match n with
    | some n => pure n.getNat
    | none => match ← scalarWidth? with
      | some w => pure w
      | none => throwError "h5i_bv: no unsigned scalar in the context; give the width: `h5i_bv 32`"
  let wT : Term := Syntax.mkNumLit (toString w)
  evalTactic (← `(tactic| bvify ($wT) at *))
  evalTactic (← `(tactic| try simp only [U8.rMax, U16.rMax, U32.rMax, U64.rMax, U128.rMax,
    Nat.reducePow, Nat.reduceSub] at *))
  evalTactic (← `(tactic| bv_decide))

end H5iAppLib
