import H5iAppLib.Basic
/-!
# `@[h5i_spec]`: the other two forms of a spec

A spec `thm : ∀ xs, f xs ⦃ r => P r ⦄` is what `step*` uses. A proof that
starts from an equation `h : f xs = ok r` wants `P r` instead, and a rewrite
wants `f xs = ok g`. `@[h5i_spec]` derives both, so neither is written (or
bridged with `post_of_ok`/`eq_ok_of_spec`) by hand:

- `thm.inv : ∀ xs r, f xs = ok r → P r`, always;
- `thm.eq : ∀ xs, f xs = ok g`, when the postcondition is `r = g`.
-/
open Aeneas Aeneas.Std Lean Meta

namespace H5iAppLib

private def addThm (nm : Name) (lps : List Name) (type value : Expr) : MetaM Unit := do
  let type ← instantiateMVars type
  let value ← instantiateMVars value
  addDecl <| .thmDecl { «name» := nm, levelParams := lps, type, value }

def deriveSpecForms (decl : Name) : MetaM Unit := do
  let info ← getConstInfo decl
  let lps := info.levelParams
  forallTelescope info.type fun xs body => do
    let body := body.consumeMData
    unless body.isAppOfArity ``WP.spec 3 do
      throwError "@[h5i_spec]: the statement of {decl} is not a spec `f ⦃ r => P r ⦄`"
    let α := body.getArg! 0
    let m := body.getArg! 1
    let P := body.getArg! 2
    let pf := mkAppN (mkConst decl (lps.map Level.param)) xs
    let rName := match P with
      | .lam n _ _ _ => if n.hasMacroScopes then `r else n
      | _ => `r
    withLocalDeclD rName α fun r => do
      withLocalDeclD `h (← mkEq m (← mkAppM ``Result.ok #[r])) fun h => do
        let v ← mkAppM ``H5iAppLib.post_of_ok #[pf, h]
        let t := (← inferType v).headBeta
        addThm (decl ++ `inv) lps (← mkForallFVars (xs ++ #[r, h]) t) (← mkLambdaFVars (xs ++ #[r, h]) v)
    -- `r = g`, with `g` not mentioning `r`.
    if let .lam _ _ b _ := P then
      if let some (_, lhs, g) := b.consumeMData.eq? then
        if lhs == .bvar 0 && !g.hasLooseBVars then
          let v ← mkAppM ``H5iAppLib.eq_ok_of_spec #[pf]
          addThm (decl ++ `eq) lps (← mkForallFVars xs (← inferType v)) (← mkLambdaFVars xs v)

initialize registerBuiltinAttribute {
  «name» := `h5i_spec
  descr := "derive `thm.inv : f xs = ok r → P r` and, for `r = g`, `thm.eq : f xs = ok g` from a spec"
  applicationTime := .afterTypeChecking
  add := fun decl _ _ => (deriveSpecForms decl).run' {} {}
}

end H5iAppLib
