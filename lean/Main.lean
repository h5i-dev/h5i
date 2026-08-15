import H5iSpec

/-!
The DRT executable: an array of `DrtInput` cases as JSON on stdin, the
model's `EffectiveConfig` for each as a JSON array on stdout. One process per
harness run, not per case. Any parse failure is a loud exit — a malformed
case must fail the harness, never skip silently.
-/

open Lean H5iSpec

def main : IO UInt32 := do
  let stdin ← IO.getStdin
  let text ← stdin.readToEnd
  match Json.parse text >>= fromJson? (α := Array DrtInput) with
  | .error e =>
    IO.eprintln s!"h5i-spec: bad input: {e}"
    return 1
  | .ok cases =>
    let out := Json.arr (cases.map (toJson ∘ computeEffective))
    IO.println out.compress
    return 0
