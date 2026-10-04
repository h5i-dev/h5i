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

end H5iAppLib
