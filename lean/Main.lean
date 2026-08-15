import H5iSpec

/-!
The model's executable face, two modes:

- **DRT** (no arguments): an array of `DrtInput` cases as JSON on stdin, the
  model's `EffectiveConfig` for each as a JSON array on stdout. One process
  per harness run, not per case.
- **Predict** (`--predict`): an `EffectiveConfig` plus a probe list on
  stdin, the full verdict per probe on stdout — `{allow, real, check}`:
  whether the mechanisms permit the access (Landlock plus nested-bind
  resolution plus read-only remounts), which host object the access
  reaches, and what must exist there for the probe to succeed. The model
  owns the semantics; `tests/effective_probes.rs` supplies the existence
  facts by stat'ing `real` on the host and holds a real box to the
  combined expectation.
- **Interferes** (`--interferes`): an array of `{a, b}` config pairs on
  stdin, `interferesCheck` over their compiled rulesets per pair on stdout
  — the oracle the Rust-side `interferes` implementation is
  differentially tested against.

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
    let out := Json.arr <| inp.probes.map fun pr =>
      let v := predictVerdict inp.config (parsePath pr.path) pr.access
      Json.mkObj [
        ("allow", Json.bool v.allow),
        ("real", Json.str ("/" ++ String.intercalate "/" v.real)),
        ("check", Json.str (match v.check with
          | .«exists» => "exists"
          | .creatable => "creatable")),
      ]
    IO.println out.compress
    return 0

/-- One interference query: does box `a` (writer) reach box `b` (reader)? -/
structure InterferesPair where
  a : EffectiveConfig
  b : EffectiveConfig
deriving FromJson

def runInterferes (text : String) : IO UInt32 := do
  match Json.parse text >>= fromJson? (α := Array InterferesPair) with
  | .error e =>
    IO.eprintln s!"h5i-spec: bad interferes input: {e}"
    return 1
  | .ok pairs =>
    let out := Json.arr <| pairs.map fun pr =>
      Json.bool (interferesCheck (compileLandlock pr.a.landlock)
        (compileLandlock pr.b.landlock))
    IO.println out.compress
    return 0

def main (args : List String) : IO UInt32 := do
  let stdin ← IO.getStdin
  let text ← stdin.readToEnd
  match args with
  | [] => runDrt text
  | ["--predict"] => runPredict text
  | ["--interferes"] => runInterferes text
  | _ =>
    IO.eprintln "usage: h5i-spec [--predict|--interferes]  (input on stdin)"
    return 2
