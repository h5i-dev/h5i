import Aeneas
/-! Facts about Aeneas scalars and clones used by every kernel. -/
open Aeneas Aeneas.Std Result

namespace H5iAppLib

theorem usize_max_le : Usize.max ≤ U64.max := by
  rw [Usize.max_def, U64.max_def]
  cases System.Platform.numBits_eq <;> simp_all [Usize.numBits, U64.numBits]

/-- `usize` has at least 32 bits. `scalar_tac` does not know it, so the side
goal `n < Usize.max` of a `push` onto a short literal is `usize_lt_max`. -/
theorem usize_max_ge : 2 ^ 32 - 1 ≤ Usize.max := by
  rw [Usize.max_def]
  cases System.Platform.numBits_eq <;> simp_all [Usize.numBits]

theorem usize_lt_max {n : Nat} (h : n < 2 ^ 32 - 1) : n < Usize.max := Nat.lt_of_lt_of_le h usize_max_ge

/-- Cloning a vector whose element clone is the identity returns the vector. -/
theorem vec_clone_eq {T : Type} (inst : core.clone.Clone T) (v : alloc.vec.Vec T)
    (h : ∀ x, inst.clone x = ok x) : alloc.vec.CloneVec.clone inst v = ok v := by
  have := Slice.clone_spec (clone := inst.clone) (s := v.slice) (fun x _ => h x)
  rw [WP.spec_equiv_exists] at this
  obtain ⟨s', hs, heq⟩ := this
  simp [alloc.vec.CloneVec.clone, hs, ← heq]

/-- Lift a property of a successful `Ok (writes, reply)` to a postcondition. -/
def OnOk {α β ε} (P : α → β → Prop) : core.result.Result (α × β) ε → Prop
  | .Ok (ws, r) => P ws r
  | .Err _ => True

theorem of_spec {α β ε} {m : Result (core.result.Result (α × β) ε)} {P : α → β → Prop}
    (hs : m ⦃ OnOk P ⦄) {ws : α} {r : β} (h : m = .ok (.Ok (ws, r))) : P ws r := by
  obtain ⟨o, ho, hp⟩ := (WP.spec_equiv_exists _ _).1 hs
  rw [h, Result.ok.injEq] at ho
  subst ho
  exact hp

theorem eq_ok_of_spec {α} {m : Result α} {v : α} (h : m ⦃ x => x = v ⦄) : m = ok v := by
  obtain ⟨y, hy, rfl⟩ := (WP.spec_equiv_exists _ _).1 h
  exact hy

/-- The postcondition holds of the value a successful computation returns. -/
theorem post_of_ok {α} {m : Result α} {P : α → Prop} {x : α} (h : m ⦃ P ⦄) (he : m = ok x) : P x := by
  obtain ⟨y, hy, hp⟩ := (WP.spec_equiv_exists _ _).1 h
  rw [he, Result.ok.injEq] at hy
  subst hy
  exact hp

/-- A computation with a spec succeeds. -/
theorem ok_of {α} {m : Result α} {P : α → Prop} (h : m ⦃ P ⦄) : ∃ r, m = ok r := by
  obtain ⟨r, hr, -⟩ := (WP.spec_equiv_exists _ _).1 h
  exact ⟨r, hr⟩

/-- Invert a successful bind: reason about a run that returned `ok` from the
equation alone, with no total spec for the callee (partial correctness). -/
theorem bind_tc_eq_ok {α β} {x : Result α} {f : α → Result β} {y : β} :
    (do let a ← x; f a) = ok y ↔ ∃ a, x = ok a ∧ f a = ok y := by
  constructor
  · intro h
    cases x using Result.cases with
    | ret a => exact ⟨a, rfl, by simpa using h⟩
    | vis e k => simp at h
    | div => simp at h
  · rintro ⟨a, rfl, h⟩; simpa using h

theorem bind_eq_ok {α β} {x : Result α} {f : α → Result β} {y : β} :
    Std.bind x f = ok y ↔ ∃ a, x = ok a ∧ f a = ok y := bind_tc_eq_ok

/-- Partial correctness of a loop: if `loop body x = ok y`, a property kept by
every successful step (with a decreasing measure) holds of `y`. No step needs
to be proven total. -/
theorem loop_ok {α β} (body : α → Result (ControlFlow α β)) (Inv : α → Prop) (Q : β → Prop) (μ : α → Nat)
    (hstep : ∀ x r, Inv x → body x = ok r → match r with
      | .done y => Q y
      | .cont x' => Inv x' ∧ μ x' < μ x) :
    ∀ x y, Inv x → loop body x = ok y → Q y := by
  intro x
  induction h : μ x using Nat.strong_induction_on generalizing x with
  | _ n ih =>
    intro y hx hl
    rw [loop] at hl
    obtain ⟨r, hr, hk⟩ := bind_eq_ok.1 hl
    have hs := hstep x r hx hr
    cases r with
    | cont x' => exact ih (μ x') (h ▸ hs.2) x' rfl y hs.1 hk
    | done y' => simp at hk; subst hk; exact hs

/-- Split an equation between two `Ok (writes, reply)` results. -/
theorem ok_inj {α β ε} {a a' : α} {b b' : β}
    (h : (core.result.Result.Ok (a', b') : core.result.Result (α × β) ε) = .Ok (a, b)) : a' = a ∧ b' = b := by
  simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at h; exact h

/-! ## Scalars

Not `@[simp]`: an app that wants one adds `attribute [simp]` itself. -/

theorem u64_val_eq (x y : U64) : x.val = y.val ↔ x = y :=
  ⟨fun h => by scalar_tac, fun h => h ▸ rfl⟩

theorem u8_eq_iff (x y : U8) : x = y ↔ x.val = y.val :=
  ⟨fun h => h ▸ rfl, fun h => by scalar_tac⟩

theorem u64_bne (x y : U64) : (x != y) = !decide (x = y) := by
  by_cases h : x = y <;> simp [h]

/-- `u64::MAX`, which `scalar_tac` does not unfold. -/
theorem u64_max_val : core.num.U64.MAX.val = 2 ^ 64 - 1 := by
  simp [core.num.U64.MAX, U64.rMax]

theorem usize_cast_u64 (n : Usize) : (UScalar.cast .U64 n).val = n.val := by
  rw [UScalar.cast_val_eq]; apply Nat.mod_eq_of_lt
  have h1 := usize_max_le; have h2 : n.val ≤ Usize.max := by scalar_tac
  rw [U64.max_def] at h1; simp [U64.numBits] at h1; simp; omega

theorem u64_cast_usize (x : U64) (h : x.val ≤ Usize.max) : (UScalar.cast .Usize x).val = x.val := by
  rw [UScalar.cast_val_eq]; apply Nat.mod_eq_of_lt
  have h2 : Usize.max < 2 ^ UScalarTy.Usize.numBits := by
    rw [Usize.max_def, Usize.numBits_def]; exact Nat.sub_lt (Nat.two_pow_pos _) Nat.one_pos
  omega

/-- `v.len() == 0`, after `Vec.len` unfolds. -/
theorem usize_ofNatCore_eq_zero (n : Nat) (h : n < 2 ^ UScalarTy.Usize.numBits) : Usize.ofNatCore n h = 0#usize ↔ n = 0 := by
  constructor
  · intro e; have := congrArg UScalar.val e; simpa using this
  · rintro rfl; rfl

/-! ## Vectors -/

/-- A concrete vector, for scenarios. -/
def vecOf {α} (l : List α) (h : l.length ≤ Usize.max := by simp only [List.length_cons, List.length_nil]; scalar_tac) :
    alloc.vec.Vec α := alloc.vec.Vec.from l h

/-- Cloning a vector whose elements clone to themselves; `h5i_derive_clone`
uses it as a conditional rewrite, so derived `Clone` specs compose. -/
theorem vec_clone_ok {T : Type} (inst : core.clone.Clone T) (v : alloc.vec.Vec T)
    (h : ∀ x, inst.clone x = ok x) : alloc.vec.CloneVec.clone inst v = ok v :=
  vec_clone_eq inst v h

theorem u8_clone (x : U8) : core.clone.CloneU8.clone x = ok x := rfl

@[simp] theorem vecOf_val {α} (l : List α) (h : l.length ≤ Usize.max) : (vecOf l h).val = l := by
  simp [vecOf]

theorem vec_new_val (α : Type) : (alloc.vec.Vec.new α).val = [] := rfl

theorem u8vec_clone (v : alloc.vec.Vec U8) : alloc.vec.CloneVec.clone core.clone.CloneU8 v = ok v :=
  vec_clone_eq _ v (fun _ => rfl)

@[step] theorem u8vec_clone_spec (v : alloc.vec.Vec U8) :
    alloc.vec.CloneVec.clone core.clone.CloneU8 v ⦃ w => w = v ⦄ := by
  simp [u8vec_clone]

theorem allM_u8 (l : List (U8 × U8)) :
    List.allM (fun (p : U8 × U8) => core.cmp.PartialEqU8.eq p.1 p.2) l =
      ok (l.all (fun p => decide (p.1 = p.2))) := by
  induction l with
  | nil => rfl
  | cons p ps ih =>
    by_cases h : p.1 = p.2
    · simp [List.allM, liftFun2, h, ih]
    · simp [List.allM, liftFun2, h, ih]; rfl

theorem zip_all_eq (a b : List U8) (h : a.length = b.length) :
    (List.zip a b).all (fun p => decide (p.1 = p.2)) = decide (a = b) := by
  induction a generalizing b with
  | nil => cases b <;> simp_all
  | cons x xs ih =>
    cases b with
    | nil => simp at h
    | cons y ys =>
      simp only [List.length_cons, Nat.add_right_cancel_iff] at h
      simp [List.zip_cons_cons, ih ys h]

/-- `==` on byte strings. -/
@[step] theorem vec_u8_eq_spec (v w : alloc.vec.Vec U8) :
    alloc.vec.partial_eq.PartialEqVec.eq core.cmp.PartialEqU8 v w ⦃ b => b = decide (v = w) ⦄ := by
  unfold alloc.vec.partial_eq.PartialEqVec.eq
  split
  · rename_i hlen
    rw [show (fun (x : U8 × U8) => match x with | (x0, x1) => core.cmp.PartialEqU8.eq x0 x1) =
        (fun p => core.cmp.PartialEqU8.eq p.1 p.2) from rfl, allM_u8]
    simp only [WP.spec_ok]
    rw [zip_all_eq _ _ hlen]; exact decide_eq_decide.2 (alloc.vec.Vec.eq_iff v w).symm
  · rename_i hlen
    simp only [WP.spec_ok]
    have : v ≠ w := fun h => hlen (by simp [h])
    simp [this]

end H5iAppLib
