import H5iSpec

/-!
The model's executable face, two modes:

- **DRT** (no arguments): an array of `DrtInput` cases as JSON on stdin, the
  model's `EffectiveConfig` for each as a JSON array on stdout. One process
  per harness run, not per case.
- **Predict** (`--predict`): an `EffectiveConfig` plus a probe list on
  stdin, the compiled ruleset's verdict per probe on stdout — the
  conformance-probe generator (§V4, "model versus kernel"): the *model*
  says what the box must allow and deny, and `tests/effective_probes.rs`
  holds a real box to it.

Any parse failure is a loud exit — a malformed case must fail the harness,
never skip silently.
-/

open Lean H5iSpec

instance : FromJson Access where
  fromJson? j := do
    match (← j.getStr?) with
    | "read" => pure .read
    | "write" => pure .write
    | other => throw s!"unknown access '{other}'"

instance : ToJson Access where
  toJson
    | .read => Json.str "read"
    | .write => Json.str "write"

/-- One conformance probe: a path and the access to ask about. -/
structure ProbeReq where
  path : String
  access : Access
deriving FromJson

/-- The predict-mode input: a box's dump and its probes. -/
structure PredictInput where
  config : EffectiveConfig
  probes : Array ProbeReq
deriving FromJson

def runDrt (text : String) : IO UInt32 := do
  match Json.parse text >>= fromJson? (α := Array DrtInput) with
  | .error e =>
    IO.eprintln s!"h5i-spec: bad input: {e}"
    return 1
  | .ok cases =>
    let out := Json.arr (cases.map (toJson ∘ computeEffective))
    IO.println out.compress
    return 0

def runPredict (text : String) : IO UInt32 := do
  match Json.parse text >>= fromJson? (α := PredictInput) with
  | .error e =>
    IO.eprintln s!"h5i-spec: bad predict input: {e}"
    return 1
  | .ok inp =>
    let rs := compileLandlock inp.config.landlock
    let out := Json.arr <| inp.probes.map fun pr =>
      Json.bool (rs.allows (parsePath pr.path) pr.access)
    IO.println out.compress
    return 0

def main (args : List String) : IO UInt32 := do
  let stdin ← IO.getStdin
  let text ← stdin.readToEnd
  match args with
  | [] => runDrt text
  | ["--predict"] => runPredict text
  | _ =>
    IO.eprintln "usage: h5i-spec [--predict]  (input on stdin)"
    return 2
