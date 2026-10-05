import H5iAppLib.Basic
import H5iAppLib.Loops
import H5iAppLib.Sets
import H5iAppLib.Inv
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

/-- The name a bind's continuation gives its value (`x` in `let x ← m; k`),
and whether it is a `Std.bind`. -/
private def bindName? (e : Lean.Expr) : Option (Lean.Name × Bool) :=
  let e := e.consumeMData
  let n := if e.isApp then
      match e.appArg!.consumeMData with
      | Lean.Expr.lam n _ _ _ => n
      | _ => `x
    else `x
  if e.isAppOfArity ``Bind.bind 6 then some (n, false)
  else if e.isAppOfArity ``Aeneas.Std.bind 4 then some (n, true)
  else none

open Lean Elab Tactic Meta in
/-- Give the inaccessible hypotheses of `g` not in `pre` names: `cond` for
propositions (the conditions `split` adds), the base name for values. -/
private def nameNew (g : MVarId) (pre : Array FVarId) (cond : Name) : MetaM MVarId := do
  let mut g := g
  for ld in (← g.getDecl).lctx do
    if ld.isImplementationDetail || pre.contains ld.fvarId || !ld.userName.hasMacroScopes then continue
    let lctx := (← g.getDecl).lctx
    let base := if ← g.withContext (isProp ld.type) then cond else ld.userName.eraseMacroScopes
    g ← g.rename ld.fvarId (lctx.getUnusedName base)
  return g

open Lean Elab Tactic Meta in
/-- One step on `h` in the main goal: simplify it, peel a bind, or split it.
False when none applies. -/
private def invertStep (h cond : Name) : TacticM Bool := withMainContext do
  let some d := (← getLCtx).findFromUserName? h | return false
  let some (_, lhs, _) := (← instantiateMVars d.type).consumeMData.eq? | return false
  let hI := mkIdent h
  let g ← getMainGoal
  let simped ← try
      evalTactic (← `(tactic| simp only [H5iAppLib.bind_branch, H5iAppLib.bind_tc_branch, H5iAppLib.from_residual_same,
        H5iAppLib.from_residual_err, ok.injEq, core.result.Result.Ok.injEq, core.result.Result.Err.injEq,
        Prod.mk.injEq, reduceCtorEq, H5iAppLib.fail_eq_ok, ↓reduceIte, ↓reduceDIte, Bool.false_eq_true, Bool.true_eq_false,
        bind_tc_ok, bind_ok] at $hI:ident))
      pure true
    catch _ => pure false
  if simped then
    let gs ← getGoals
    if gs.isEmpty then return true
    -- Reduced to `True`: nothing left to invert.
    withMainContext do
      if let some d' := (← getLCtx).findFromUserName? h then
        if (← instantiateMVars d'.type).consumeMData.isConstOf ``True then
          replaceMainGoal [← (← getMainGoal).clear d'.fvarId]
    return gs != [g]
  if let some (n, std) := bindName? lhs then
    let base := if n.hasMacroScopes || n.isAnonymous then `x else n
    let lctx ← getLCtx
    let x := lctx.getUnusedName base
    let hx := lctx.getUnusedName (.mkSimple ("h" ++ x.eraseMacroScopes.toString))
    let thm := mkIdent (if std then ``H5iAppLib.bind_eq_ok else ``H5iAppLib.bind_tc_eq_ok)
    evalTactic (← `(tactic| obtain ⟨$(mkIdent x):ident, $(mkIdent hx):ident, $hI:ident⟩ := ($thm).1 $hI))
    -- `obtain` keeps the old `h`, shadowed.
    replaceMainGoal [← (← getMainGoal).clear d.fvarId]
    return true
  let pre := (← getLCtx).getFVarIds
  try evalTactic (← `(tactic| split at $hI:ident)) catch _ => return false
  setGoals (← (← getGoals).mapM (nameNew · pre cond))
  return true

open Lean Elab Tactic Meta in
/-- A path that ends in `h : a = b` with a variable on one side: substitute
it, the variable `h5i_invert` introduced first, else the other. -/
private def substEnd (h : Name) (pre : Array FVarId) : TacticM Unit := do
  let mut out := #[]
  for g in ← getGoals do
    setGoals [g]
    let vars ← withMainContext do
      let some d := (← getLCtx).findFromUserName? h | return []
      let some (_, a, b) := (← instantiateMVars d.type).consumeMData.eq? | return []
      let vs := [a, b].filter (·.isFVar)
      let vs := vs.filter (fun e => !pre.contains e.fvarId!) ++ vs.filter (fun e => pre.contains e.fvarId!)
      vs.mapM fun v => return (← v.fvarId!.getDecl).userName
    for v in vars do
      if ← (try evalTactic (← `(tactic| subst $(mkIdent v):ident)); pure true catch _ => pure false) then break
    out := out ++ (← getGoals)
  setGoals out.toList

open Lean Elab Tactic in
private partial def invertGoals (h cond : Name) : Nat → TacticM Unit
  | 0 => pure ()
  | fuel + 1 => do
    let mut out := #[]
    for g in ← getGoals do
      setGoals [g]
      if ← invertStep h cond then invertGoals h cond fuel
      out := out ++ (← getGoals)
    setGoals out.toList

/-- `h5i_invert h` for `h : f x = ok y` (with `f` unfolded): peel the binds,
split the branches, and drop the ones that contradict `h`. What is left are
the successful paths, and a path that ends in `h : x = e` substitutes it.
Each `let x ← m` on the path leaves `x` and
`hx : m = ok x` under the names the code gives them; each branch condition is
`hc` (`h5i_invert h with hcond` names it), and `?` splits into its `Ok` and
`Err` cases. Partial correctness: no callee needs a total spec. -/
syntax "h5i_invert " ident (" with " ident)? : tactic

open Lean Elab Tactic in
elab_rules : tactic
  | `(tactic| h5i_invert $h $[with $c]?) => do
    let pre ← withMainContext do return (← getLCtx).getFVarIds
    invertGoals h.getId (c.map (·.getId) |>.getD `hc) 256
    substEnd h.getId pre

/-- What the `= ok` equations of arithmetic and indexing say about values. -/
private def okFacts : List Lean.Name :=
  [``H5iAppLib.add_ok_val, ``H5iAppLib.add_ok_bound, ``H5iAppLib.sub_ok_val, ``H5iAppLib.mul_ok_val,
   ``H5iAppLib.div_ok_val, ``H5iAppLib.rem_ok_val, ``H5iAppLib.vec_index_ok_get?,
   ``H5iAppLib.vec_index_ok_mem, ``H5iAppLib.vec_index_slice_ok_get?, ``H5iAppLib.vec_index_slice_ok_mem,
   ``H5iAppLib.slice_index_ok_mem]

open Lean Elab Tactic Meta in
/-- `h5i_ok_facts`: for every hypothesis `h : x + y = ok z` (also `-`, `*`,
`/`, `%`, and `index_usize`), add what it says about values
(`z.val = x.val + y.val`, `v.val[i.val]? = some x`, ...) as `h_val`. -/
elab "h5i_ok_facts" : tactic => withMainContext do
  for d in ← getLCtx do
    if d.isImplementationDetail then continue
    unless (← instantiateMVars d.type).consumeMData.isAppOfArity ``Eq 3 do continue
    for thm in okFacts do
      let g ← getMainGoal
      discard <| observing? <| g.withContext do
        let pf ← mkAppM thm #[d.toExpr]
        let ty ← inferType pf
        let nm := (← getLCtx).getUnusedName (d.userName.eraseMacroScopes.appendAfter "_val")
        let (_, g') ← (← g.assert nm ty pf).intro1P
        replaceMainGoal [g']

/-- `h5i_arith`: `h5i_ok_facts`, unfold `saturating_sub`/`saturating_add`
and widening casts, then `scalar_tac`. For goals about the values a path
computed with `i + 1#usize = ok j` and the like. -/
macro "h5i_arith" : tactic => `(tactic| (
  h5i_ok_facts
  try subst_vars
  try simp only [H5iAppLib.saturating_sub_val, H5iAppLib.saturating_add_val,
    core.num.U8.saturating_sub, core.num.U16.saturating_sub, core.num.U32.saturating_sub,
    core.num.U64.saturating_sub, core.num.U128.saturating_sub, core.num.Usize.saturating_sub,
    core.num.U8.saturating_add, core.num.U16.saturating_add, core.num.U32.saturating_add,
    core.num.U64.saturating_add, core.num.U128.saturating_add, core.num.Usize.saturating_add] at *
  scalar_tac))

/-- `h5i_derive_eq T f`: derive `DecidableEq T` and a `@[step]` spec saying the
extracted `==` of `T` (`f`, e.g. `T.Insts.CoreCmpPartialEqT.eq`) decides equality.
Derived `==` compares enums by `read_discriminant`, which the WP tactics do not
reduce. If the `PartialEq` instance was extracted too, it also gets an `EqLaw`
instance, so `h5i-app-std`'s `set`/`map` specs apply to `Vec<T>`.
Run it for field types first. -/
syntax "h5i_derive_eq " ident ident : command
open Lean Elab Command in
elab_rules : command
  | `(h5i_derive_eq $t $f) => do
    let thm := mkIdent (f.getId ++ `spec)
    let disc := mkIdent (t.getId ++ `read_discriminant)
    elabCommand (← `(deriving instance DecidableEq for $t))
    elabCommand (← `(@[step] theorem $thm (a b : $t) : $f a b ⦃ r => r = decide (a = b) ⦄ := by
        unfold $f
        first
          | (cases a <;> cases b <;> simp [$disc:ident])
          | (cases a <;> cases b <;> (repeat' (first | step | split)) <;> simp_all)))
    let full ← liftCoreM <| realizeGlobalConstNoOverloadWithInfo f
    unless (← getEnv).contains full.getPrefix do return
    let inst := mkIdent full.getPrefix
    -- `ne` is the trait default, or the structure's default field: unfold either.
    elabCommand (← `(set_option linter.unusedSimpArgs false in
      instance : H5iAppLib.EqLaw $inst where
        eq_ok a b := H5iAppLib.eq_ok_of_spec ($thm a b)
        ne_ok a b := by
          simp only [$inst:ident, core.cmp.PartialEq.ne.trait_default, core.cmp.PartialEq.ne.default,
            H5iAppLib.eq_ok_of_spec ($thm a b)]
          try simp))

/-- `h5i_derive_clone T f`: `f x = ok x` (`@[simp]`) and `@[step]` specs for
`f`, the extracted derived `clone` of `T` (e.g. `T.Insts.CoreCloneClone.clone`),
and for cloning a `Vec` of `T`, with a `CloneLaw` instance for the latter.
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
    elabCommand (← `(instance : H5iAppLib.CloneLaw $inst := ⟨fun x => $eqThm x⟩))

/-- Library roots `h5i_derive_all` leaves alone. -/
private def libraryRoots : List Lean.Name :=
  [`Init, `Lean, `Std, `Lake, `Aeneas, `H5iAppLib, `Mathlib, `Batteries, `Aesop, `Qq, `Plausible,
   `ProofWidgets, `ImportGraph, `LeanSearchClient, `Cli]

/-- `(T, f, isEq)` for each extracted `T.Insts.CoreCmpPartialEq*.eq` and
`T.Insts.CoreCloneClone.clone` outside the libraries. -/
private def deriveCandidates (env : Lean.Environment) : Array (Lean.Name × Lean.Name × Bool) := Id.run do
  let mut out := #[]
  -- Only the modules outside the libraries: scanning Mathlib's constants is
  -- most of a minute.
  for i in [:env.header.moduleNames.size] do
    if libraryRoots.contains env.header.moduleNames[i]!.getRoot then continue
    for c in env.header.moduleData[i]!.constNames do
      -- `T.Insts.<Inst>.<method>`
      let .str (.str (.str t "Insts") inst) m := c | continue
      let isEq := m == "eq" && inst.startsWith "CoreCmpPartialEq"
      unless isEq || (m == "clone" && inst == "CoreCloneClone") do continue
      let some (.inductInfo _) := env.find? t | continue
      out := out.push (t, c, isEq)
  return out

/-- `h5i_derive_all`: `h5i_derive_eq` and `h5i_derive_clone` for every type
of the extraction with an extracted `PartialEq` or `Clone`, field types
first. A type it cannot derive (a field without the instance) is skipped and
named in a note. Put it once after the imports. -/
syntax "h5i_derive_all" : command
open Lean Elab Command in
elab_rules : command
  | `(h5i_derive_all) => do
    let env ← getEnv
    let mut todo := (deriveCandidates env).filter fun (_, f, _) => !env.contains (f ++ `spec)
    let mut progress := true
    while progress && !todo.isEmpty do
      progress := false
      let mut left := #[]
      for (t, f, isEq) in todo do
        let saved ← get
        let tI := mkIdent (`_root_ ++ t)
        let fI := mkIdent (`_root_ ++ f)
        let cmd ← if isEq then `(h5i_derive_eq $tI $fI) else `(h5i_derive_clone $tI $fI)
        -- Synchronous, so a failed proof is an error here, not later.
        try withScope (fun sc => { sc with opts := sc.opts.setBool `Elab.async false }) (elabCommand cmd)
        catch _ => pure ()
        if (← get).messages.hasErrors && !saved.messages.hasErrors then
          set saved
          left := left.push (t, f, isEq)
        else
          progress := true
      todo := left
    for (t, _, isEq) in todo do
      logInfo m!"h5i_derive_all: no {if isEq then "eq" else "clone"} spec for {t}; derive it by hand"

/-- `h5i_when f => cmd`: elaborate `cmd` only if `f` names a declaration.
A kernel's extraction holds only the functions of an included crate that the
kernel calls, so a copied spec file guards each spec by its function. -/
syntax "h5i_when " ident " => " command : command
open Lean Elab Command in
elab_rules : command
  | `(h5i_when $f => $cmd) => do
    let found ← try
        discard <| liftCoreM <| realizeGlobalConstNoOverload f
        pure true
      catch _ => pure false
    if found then elabCommand cmd

/-- Simplify the leftovers of `step*` and `split` (`if false = true`, `id`,
`ok` binds) without failing when nothing changes. -/
macro "h5i_simp" : tactic => `(tactic| (
  simp -failIfUnchanged only [Bool.false_eq_true, Bool.true_eq_false, ↓reduceIte, ↓reduceDIte,
    ite_true, ite_false, if_true, if_false, id_eq, _root_.id, bind_ok, bind_tc_ok, WP.spec_ok,
    decide_true, decide_false, Bool.not_true, Bool.not_false] at *))

/-- Close the per-step goal of `loop_search` or `loop_fold`. -/
macro "h5i_step" : tactic => `(tactic| (
  step* <;> (repeat' (first | step | split)) <;>
    (try simp only [H5iAppLib.SearchStep, H5iAppLib.FoldStep, H5iAppLib.FoldStep2]) <;> (try simp_all) <;> try scalar_tac))

/-- `h5i_step` with extra simp lemmas, for a loop predicate that is a named def. -/
macro "h5i_step" " [" ls:Lean.Parser.Tactic.simpLemma,* "]" : tactic => `(tactic| (
  step* <;> (repeat' (first | step | split)) <;>
    (try simp only [H5iAppLib.SearchStep, H5iAppLib.FoldStep, H5iAppLib.FoldStep2]) <;> (try simp_all [$ls,*]) <;> try scalar_tac))

open Lean Elab Tactic Meta in
/-- Unfold the Aeneas loop body (`*_loop.body`) the goal mentions. -/
elab "h5i_unfold_body" : tactic => withMainContext do
  let t ← instantiateMVars (← getMainTarget)
  let some c := t.find? (fun e => e.isConst && e.constName!.lastComponentAsString == "body")
    | throwError "h5i_unfold_body: no `*.body` in the goal"
  evalTactic (← `(tactic| unfold $(mkIdent c.constName!):ident))

/-- `h5i_search_any l P` on `loop body 0#usize ⦃ b => ... ⦄` (after
`unfold f f_loop`), for a loop that returns `true` at the first element of
`l` satisfying `P` and `false` at the end: the loop returns `l.any P`, and
each step is closed with `h5i_step`. What is left: the goal with `b`
replaced by `l.any P`, and any step goal `h5i_step` could not close. -/
macro "h5i_search_any " l:term:max P:term:max : tactic => `(tactic| (
  apply WP.spec_mono (H5iAppLib.loop_search $l $P id (fun _ _ => true) false _ ?step 0#usize (by simp))
  on_goal 1 => (intro r hr; simp only [id] at hr; have h := H5iAppLib.search_any _ _ _ hr; subst h; try simp_all)
  case' step => (intro j hj; h5i_unfold_body; h5i_step)))

/-- `h5i_search_all l P`: the loop returns `false` at the first element
satisfying `P` and `true` at the end, so it returns `l.all (fun x => !P x)`. -/
macro "h5i_search_all " l:term:max P:term:max : tactic => `(tactic| (
  apply WP.spec_mono (H5iAppLib.loop_search $l $P id (fun _ _ => false) true _ ?step 0#usize (by simp))
  on_goal 1 => (intro r hr; simp only [id] at hr; have h := H5iAppLib.search_all _ _ _ hr; subst h; try simp_all)
  case' step => (intro j hj; h5i_unfold_body; h5i_step)))

/-- `h5i_search_find l P`: the loop returns `some x` for the first element
satisfying `P` and `none` at the end, so it returns `l.find? P`. -/
macro "h5i_search_find " l:term:max P:term:max : tactic => `(tactic| (
  apply WP.spec_mono (H5iAppLib.loop_search $l $P id (fun _ x => some x) none _ ?step 0#usize (by simp))
  on_goal 1 => (intro r hr; simp only [id] at hr; have h := H5iAppLib.search_find _ _ _ hr; subst h; try simp_all)
  case' step => (intro j hj; h5i_unfold_body; h5i_step)))

/-- `h5i_total idx n` on `loop body x ⦃ post ⦄`: the measure is
`n - (idx s).val`, so each step that continues must move `idx` forward and
keep it at most `n` (`loop_idx_spec`). Steps go to `h5i_step`. For a plain
`while i < v.len()` loop, `h5i_total (fun i => i) v.length`. -/
macro "h5i_total " idx:term:max n:term:max : tactic => `(tactic| (
  apply H5iAppLib.loop_idx_spec _ $idx $n (fun _ => True) _ ?step _ trivial
  case' step => (intro x _ hx; h5i_unfold_body; h5i_step)
  all_goals try scalar_tac))

open Lean Elab Tactic Meta in
/-- `h5i_measure_induction e with ih`: strong induction on the number `e`
(say `p.length - pi + (n.length - ni)` for a function recursing on indexes
into slices), generalizing everything else. `ih` covers every state with a
smaller measure. Recursion on indexes does not follow `cases` on the list;
this does. -/
elab "h5i_measure_induction " e:term " with " ih:ident : tactic => withMainContext do
  let k := (← getLCtx).getUnusedName `k
  let hm := (← getLCtx).getUnusedName `hm
  evalTactic (← `(tactic| generalize $(mkIdent hm):ident : $e = $(mkIdent k):ident))
  withMainContext do
    let some kd := (← getLCtx).findFromUserName? k | throwError "h5i_measure_induction: lost {k}"
    -- What mentions `k` (`hm`) is generalized anyway.
    let others := (← getLCtx).foldl (init := #[]) fun acc d =>
      if d.isImplementationDetail || d.userName == k || d.type.containsFVar kd.fvarId then acc
      else acc.push (mkIdent d.userName)
    evalTactic (← `(tactic| induction $(mkIdent k):ident using Nat.strong_induction_on generalizing $others*))
    let bk : TSyntax ``Lean.binderIdent := ⟨mkNode ``Lean.binderIdent #[mkIdent k]⟩
    let bih : TSyntax ``Lean.binderIdent := ⟨mkNode ``Lean.binderIdent #[ih]⟩
    evalTactic (← `(tactic| rename_i $bk $bih))

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
