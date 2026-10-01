import H5iAppLib.Basic
import H5iAppLib.Loops
/-! Tactics for kernel proofs. -/
open Aeneas Aeneas.Std Result

namespace H5iAppLib

/-- `let x = if c { a } else { b };` extracts as a bind on `if c then ok a else ok b`,
where `step*` otherwise stops. Not a global `@[step]` (it changes what `walk`
leaves); use `attribute [local step] H5iAppLib.ite_ok_spec`, or `h5i_steps`. -/
theorem ite_ok_spec {α} (c : Prop) [Decidable c] (a b : α) :
    (if c then ok a else ok b) ⦃ x => x = if c then a else b ⦄ := by
  split <;> simp

/-- `let x = if c { f() } else { g() };` with calls in the branches: `step*` stops
at the bind; rewriting with this puts the `if` on top, where `step*` splits it. -/
theorem bind_tc_ite {α β} (c : Prop) [Decidable c] (m₁ m₂ : Result α) (k : α → Result β) :
    (do let x ← (if c then m₁ else m₂ : Result α); k x) =
      if c then (do let x ← m₁; k x) else (do let x ← m₂; k x) := by
  split <;> rfl

theorem bind_ite {α β} (c : Prop) [Decidable c] (m₁ m₂ : Result α) (k : α → Result β) :
    Std.bind (if c then m₁ else m₂) k = if c then Std.bind m₁ k else Std.bind m₂ k := by
  split <;> rfl

/-- `step*`, also through binds on an `if` whose branches call functions, and on
a `match` (split when `step*` stops). -/
macro "h5i_steps" : tactic => `(tactic| (
  step*
  all_goals (repeat' (first
    | (simp only [H5iAppLib.bind_tc_ite, H5iAppLib.bind_ite]; step*)
    | (split <;> step*)))))

/-- `h5i_invert h` for `h : f x = ok y` (with `f` unfolded): peel binds with
`bind_tc_eq_ok`, name nothing, split branches, and drop the branches that
contradict `h`. What is left are the successful paths, each with the
equations of its calls as hypotheses. Partial correctness: no callee needs a
total spec. -/
macro "h5i_invert " h:ident : tactic => `(tactic| (
  repeat' (first
    | (simp only [H5iAppLib.bind_tc_eq_ok, H5iAppLib.bind_eq_ok, ok.injEq, core.result.Result.Ok.injEq,
        core.result.Result.Err.injEq, Prod.mk.injEq, reduceCtorEq, false_and, and_false, exists_false,
        exists_and_left, exists_and_right, exists_eq_left, exists_eq_right] at $h:ident)
    | (obtain ⟨_, _, $h:ident⟩ := $h)
    | (split at $h:ident))))

/-- `h5i_derive_eq T f`: derive `DecidableEq T` and a `@[step]` spec saying the
extracted `==` of `T` (`f`, e.g. `T.Insts.CoreCmpPartialEqT.eq`) decides equality.
Derived `==` compares enums by `read_discriminant`, which the WP tactics do not
reduce. Run it for field types first. -/
syntax "h5i_derive_eq " ident ident : command
macro_rules
  | `(h5i_derive_eq $t $f) => do
    let thm := Lean.mkIdent (f.getId ++ `spec)
    let disc := Lean.mkIdent (t.getId ++ `read_discriminant)
    `(deriving instance DecidableEq for $t
      @[step] theorem $thm (a b : $t) : $f a b ⦃ r => r = decide (a = b) ⦄ := by
        unfold $f
        first
          | (cases a <;> cases b <;> simp [$disc:ident])
          | (cases a <;> cases b <;> (repeat' (first | step | split)) <;> simp_all))

/-- `h5i_derive_clone T f`: `f x = ok x` (`@[simp]`) and `@[step]` specs for
`f`, the extracted derived `clone` of `T` (e.g. `T.Insts.CoreCloneClone.clone`),
and for cloning a `Vec` of `T`.
Run it for field types first. -/
syntax "h5i_derive_clone " ident ident : command
open Lean Elab Command in
elab_rules : command
  | `(h5i_derive_clone $t $f) => do
    let eqThm := mkIdent (f.getId ++ `ok_eq)
    let specThm := mkIdent (f.getId ++ `spec)
    let vecThm := mkIdent (f.getId ++ `vec_spec)
    let inst := mkIdent f.getId.getPrefix
    elabCommand (← `(@[simp] theorem $eqThm (x : $t) : $f x = ok x := by
        cases x <;> simp [$f:ident, lift, H5iAppLib.vec_clone_ok, H5iAppLib.u8_clone]))
    elabCommand (← `(@[step] theorem $specThm (x : $t) : $f x ⦃ y => y = x ⦄ := by
        simp [$eqThm:ident]))
    -- Only if the `Clone` instance was extracted too.
    let full ← liftCoreM <| realizeGlobalConstNoOverloadWithInfo f
    unless (← getEnv).contains full.getPrefix do return
    elabCommand (← `(@[step] theorem $vecThm (v : alloc.vec.Vec $t) :
        alloc.vec.CloneVec.clone $inst v ⦃ w => w = v ⦄ := by
        simp [H5iAppLib.vec_clone_ok $inst v (fun x => $eqThm x)]))

/-- Simplify the leftovers of `step*` and `split` (`if false = true`, `id`,
`ok` binds) without failing when nothing changes. -/
macro "h5i_simp" : tactic => `(tactic| (
  simp -failIfUnchanged only [Bool.false_eq_true, Bool.true_eq_false, ↓reduceIte, ↓reduceDIte,
    ite_true, ite_false, if_true, if_false, id_eq, _root_.id, bind_ok, bind_tc_ok, WP.spec_ok,
    decide_true, decide_false, Bool.not_true, Bool.not_false] at *))

/-- Close the per-step goal of `loop_search` or `loop_fold`. -/
macro "h5i_step" : tactic => `(tactic| (
  step* <;> (repeat' (first | step | split)) <;>
    (try simp only [H5iAppLib.SearchStep, H5iAppLib.FoldStep]) <;> (try simp_all) <;> try scalar_tac))

/-- `h5i_step` with extra simp lemmas, for a loop predicate that is a named def. -/
macro "h5i_step" " [" ls:Lean.Parser.Tactic.simpLemma,* "]" : tactic => `(tactic| (
  step* <;> (repeat' (first | step | split)) <;>
    (try simp only [H5iAppLib.SearchStep, H5iAppLib.FoldStep]) <;> (try simp_all [$ls,*]) <;> try scalar_tac))

/-- Run `f` symbolically, one goal per path. -/
syntax "walk " ident : tactic
macro_rules
  | `(tactic| walk $f) => `(tactic| (
    unfold $f:ident
    split <;> step* <;> (repeat' (first | step | split | simp only [WP.spec_ok, bind_tc_ok, bind_ok]))
    all_goals (try (rename_i v _; cases v))
    all_goals (try (first | dsimp only | (split; dsimp only)))
    all_goals (repeat' (first | step | split | simp only [WP.spec_ok, bind_tc_ok, bind_ok]))))

/-- Evaluate a concrete extracted kernel call; the lemmas reduce the concrete data. -/
macro "h5i_eval" " (" f:ident g:ident ")" " [" ls:Lean.Parser.Tactic.simpLemma,* "]" : tactic => `(tactic| (
  unfold $f $g
  step*
  all_goals (simp_all [$ls,*])
  all_goals (try scalar_tac)))

/-- Variant for a command whose concrete run calls a third named helper. -/
macro "h5i_eval" " (" f:ident g:ident h:ident ")" " [" ls:Lean.Parser.Tactic.simpLemma,* "]" : tactic => `(tactic| (
  unfold $f $g $h
  step*
  all_goals (simp_all [$ls,*])
  all_goals (try scalar_tac)))

end H5iAppLib
