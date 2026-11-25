import Lean
import Veil

/-!
# Type-Checked Temporal Verification Gate (`H5iAppVeil.Gate`)

Reusable Lean/Veil gate commands invoked by `h5i app temporal-check`:

* `H5iAppVeil.Gate.checkTheorem`: verifies that a named declaration exists in
  the elaborated Lean environment, is an actual theorem (`ci.isTheorem`, rejecting
  `def` / `axiom` / `opaque`), depends only on the allowed standard axioms, and
  (when `stmt?` is provided) has a type definitionally equal (`Kernel.isDefEq`)
  to the parsed and elaborated expected proposition.
* `H5iAppVeil.Gate.runVeilCheck`: executes a Veil module's finite-state safety
  model checker (`Veil.ModelChecker.Concrete.findReachableThen`) or Lasso
  liveness checker (`Veil.LivenessMC.checkAllWithProgress`) directly via Veil's
  structured Lean API (`Veil.getExecutableModule?`) and prints the structured
  JSON result tagged with a per-run delimiter.

## Trust Boundary Note
The per-run tag prevents accidental collisions with normal Lean/Veil output in
local developer and trusted-CI workflows. Because the gate runs in the same
Lean process and filesystem as the imported proof modules, it is **not** a
security boundary against arbitrary untrusted Lean metaprograms.
-/

namespace H5iAppVeil.Gate

open Lean Elab Command

/-- Verify that `declName` is a Lean theorem (`ci.isTheorem`), depends only on
`std` axioms, and matches `stmt?` via `Kernel.isDefEq` when provided. Emits one
structured JSON line prefixed with `tag`. -/
def checkTheorem (declName : Name) (stmt? : Option String) (std : List Name) (tag : String) :
    CommandElabM Unit := do
  let env ← getEnv
  let emit (status : String) (extra : List (String × Json) := []) : CommandElabM Unit := do
    let row := Json.mkObj ([("name", toJson declName.toString), ("status", toJson status)] ++ extra)
    liftIO <| IO.println (tag ++ " " ++ row.compress)
  match env.find? declName with
  | none => emit "missing" [("detail", toJson "declaration not found in built environment")]
  | some ci =>
    if !ci.isTheorem then
      let kind := match ci with
        | .defnInfo _ => "def"
        | .axiomInfo _ => "axiom"
        | .opaqueInfo _ => "opaque"
        | .inductInfo _ => "inductive"
        | .ctorInfo _ => "constructor"
        | .recInfo _ => "recursor"
        | .quotInfo _ => "quot"
        | .thmInfo _ => "theorem"
      emit "not_theorem" [("detail", toJson s!"expected a theorem, found `{kind}`")]
      return
    let axs ← liftTermElabM (Lean.collectAxioms declName)
    let extraAxs := axs.filter (fun a => !std.contains a)
    unless extraAxs.isEmpty do
      emit "bad_axioms" [
        ("axioms", toJson (axs.toList.map Name.toString)),
        ("detail", toJson s!"disallowed axioms: {String.intercalate ", " (extraAxs.toList.map Name.toString)}")
      ]
      return
    match stmt? with
    | none =>
      emit "ok" [("axioms", toJson (axs.toList.map Name.toString))]
    | some stmtSrc =>
      match Parser.runParserCategory env `term stmtSrc with
      | .error err =>
        emit "statement_error" [("detail", toJson s!"failed to parse expected statement: {err}")]
      | .ok stx =>
        let verdict ← liftTermElabM do
          try
            let expected ← Term.elabTerm stx (some (.sort .zero))
            Term.synthesizeSyntheticMVarsNoPostponing
            let expected ← instantiateMVars expected
            if expected.hasMVar then
              return ("statement_error", s!"expected statement contains unresolved metavariables: {(← Meta.ppExpr expected).pretty}")
            match Kernel.isDefEq env {} expected ci.type with
            | .ok true => return ("ok", "")
            | _ =>
              let expStr := (← Meta.ppExpr expected).pretty
              let gotStr := (← Meta.ppExpr ci.type).pretty
              return ("statement_mismatch", s!"expected `{expStr}`, found `{gotStr}`")
          catch ex =>
            return ("statement_error", ← ex.toMessageData.toString)
        if verdict.1 == "ok" then
          emit "ok" [("axioms", toJson (axs.toList.map Name.toString))]
        else
          emit verdict.1 [("detail", toJson verdict.2)]

/-- Execute Veil's finite-state safety model checker (`propName? = none`) or
Lasso temporal checker (`propName? = some p`) on `modName` using `instSrc` and
`theorySrc`, emitting one structured JSON line prefixed with `tag`. -/
def runVeilCheck (label : String) (modName : Name) (propName? : Option Name)
    (instSrc theorySrc : String) (tag : String) : CommandElabM Unit := withScope id do
  let env ← getEnv
  let emitErr (msg : String) : CommandElabM Unit := do
    let row := Json.mkObj [("name", toJson label), ("status", toJson "failed"), ("detail", toJson msg)]
    liftIO <| IO.println (tag ++ " " ++ row.compress)
  let some em ← Veil.getExecutableModule? modName
    | emitErr s!"module `{modName}` has no Veil executable definitions (add #gen_executable or #model_check)"
      return
  let .ok instStx := Parser.runParserCategory env `term instSrc
    | emitErr s!"failed to parse instantiation: {instSrc}"
      return
  let .ok theoryStx := Parser.runParserCategory env `term theorySrc
    | emitErr s!"failed to parse theory: {theorySrc}"
      return
  let instTerm : Term := ⟨instStx⟩
  let theoryTerm : Term := ⟨theoryStx⟩
  let modIdent := mkIdent modName
  let qName (n : Name) := mkIdent (em.name ++ n)
  let instId := mkIdent `inst
  let thId := mkIdent `th
  let instSortArgs ← em.instantiationFields.mapM fun f => `($instId.$(mkIdent f))
  let mkAssump (n : Name) : CommandElabM Term :=
    `(Veil.ModelChecker.TheoryProperty.mk $(quote n) (fun th => $(qName n) th))
  let mkInv (n : Name) : CommandElabM Term :=
    `(Veil.ModelChecker.SafetyProperty.mk $(quote n) (fun th st => $(qName n) th st))
  let assumpList ← `([$((← em.assumptions.mapM mkAssump)),*])
  let constraintList ← `([$((← em.stateConstraints.mapM mkInv)),*])
  let sysTerm ← `(letI : Veil.LogSwitch := ⟨false⟩; $(qName `enumerableTransitionSystem) $instSortArgs* $thId)
  let pickTerm ← `(fun _ => letI : Veil.LogSwitch := ⟨true⟩; $(qName `pickSource) $instSortArgs* $thId)
  let earlyTerm ← `([Veil.ModelChecker.EarlyTerminationCondition.foundViolatingState,
                     Veil.ModelChecker.EarlyTerminationCondition.assertionFailed,
                     Veil.ModelChecker.EarlyTerminationCondition.deadlockOccurred])
  let evalTerm ← match propName? with
    | none =>
      let invList ← `([$((← em.invariants.mapM mkInv)),*])
      let termProp ← match em.terminations[0]? with
        | some t => mkInv t
        | none => `(default)
      `(veil_dsimp_field_reads% (
          let $instId : $(qName `Instantiation) := __veil_instantiation_of% $modIdent $instTerm
          let $thId : $(qName `Theory) $instSortArgs* := $theoryTerm
          show IO Unit from do
            let sp : Veil.ModelChecker.SearchParameters _ _ := {
              assumptions := $assumpList,
              invariants := $invList,
              terminating := $termProp,
              stateConstraints := $constraintList,
              earlyTerminationConditions := $earlyTerm
            }
            let tk ← IO.CancelToken.new
            let json : Lean.Json ← Veil.ModelChecker.Concrete.findReachableThen
              (σₕ := UInt64) (ShardOf := fun v => Std.HashMap UInt64 v)
              $sysTerm $pickTerm sp none 0 tk (fun res => (pure (Lean.toJson res) : IO Lean.Json))
            let row := Lean.Json.mkObj [("name", Lean.toJson $(quote label)), ("kind", Lean.toJson "safety"), ("veil", json)]
            IO.println ($(quote tag) ++ " " ++ row.compress)))
    | some propName =>
      let astId := qName (Veil.toTemporalAstName propName)
      `(veil_dsimp_field_reads% (
          let $instId : $(qName `Instantiation) := __veil_instantiation_of% $modIdent $instTerm
          let $thId : $(qName `Theory) $instSortArgs* := $theoryTerm
          show IO Unit from do
            let sp : Veil.ModelChecker.SearchParameters _ _ := {
              assumptions := $assumpList,
              invariants := [],
              terminating := default,
              stateConstraints := $constraintList,
              earlyTerminationConditions := $earlyTerm
            }
            let p := $astId
              ($(qName `Theory) $instSortArgs*)
              ($(qName `State) ($(qName `FieldConcreteType) $instSortArgs*))
              $instSortArgs*
              ($(qName `FieldConcreteType) $instSortArgs*)
            let tk ← IO.CancelToken.new
            let json ← Veil.LivenessMC.checkAllWithProgress $sysTerm sp [p] 0 tk .fair
            let row := Lean.Json.mkObj [("name", Lean.toJson $(quote label)), ("kind", Lean.toJson "temporal"), ("veil", json)]
            IO.println ($(quote tag) ++ " " ++ row.compress)))
  try
    let callCmd ← `(command| open Veil $(mkIdent em.name) in #eval $evalTerm)
    elabCommand callCmd
  catch ex =>
    emitErr (← ex.toMessageData.toString)

end H5iAppVeil.Gate
