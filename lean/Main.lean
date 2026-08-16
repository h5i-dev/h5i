import H5iSpec
import H5iFs

/-!
The model's executable face, four modes:

- **Predict** (`--predict`): an `EffectiveConfig` plus a probe list on
  stdin, the full verdict per probe on stdout — `{allow, real, check}`:
  whether the mechanisms permit the access (Landlock plus nested-bind
  resolution plus read-only remounts), which host object the access
  reaches, and what must exist there for the probe to succeed. The model
  owns the semantics; `tests/effective_probes.rs` supplies the existence
  facts by stat'ing `real` on the host and holds a real box to the
  combined expectation.
- **Seatbelt** (`--seatbelt`): a `SeatbeltInput` on stdin, the modeled
  file-rule fragment of the generated SBPL profile on stdout —
  `tests/seatbelt_drt.rs` parses the same rules out of the Rust
  generator's text and diffs the structures.
- **Interferes** (`--interferes`): an array of `{a, b}` config pairs on
  stdin, `interferesCheck` over their compiled rulesets per pair on stdout
  — the oracle the Rust-side `interferes` implementation is
  differentially tested against.
- **Validate** (`--validate`): an array of validator cases on stdin
  (`{policy, world, plan}`), the `H5iFs.validate` verdict per case as a JSON
  bool array on stdout — the oracle the Rust-side validator port is
  differentially tested against (`tests/validate_drt.rs`). The world is the
  measured `FsState` (nodes/entries/content/root); the plan is the shipped
  grant lists (`ro`/`rw` component paths).

Any parse failure is a loud exit — a malformed case must fail the harness,
never skip silently.
-/

open Lean H5iSpec H5iFs

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

instance : FromJson SymlinkFact where
  fromJson? j := do
    pure
      { link := (← j.getObjValAs? String "link")
        target := (← j.getObjValAs? String "target") }

/-- The predict-mode input: a box's dump, its probes, and the symlink facts
the harness measured (absent means none). -/
structure PredictInput where
  config : EffectiveConfig
  probes : Array ProbeReq
  symlinks : Option (Array SymlinkFact)
deriving FromJson

def runPredict (text : String) : IO UInt32 := do
  match Json.parse text >>= fromJson? (α := PredictInput) with
  | .error e =>
    IO.eprintln s!"h5i-spec: bad predict input: {e}"
    return 1
  | .ok inp =>
    let links := (inp.symlinks.getD #[]).toList
    let out := Json.arr <| inp.probes.map fun pr =>
      let v := predictVerdict inp.config links (parsePath pr.path) pr.access
      Json.mkObj [
        ("allow", Json.bool v.allow),
        ("real", Json.str ("/" ++ String.intercalate "/" v.real)),
        ("check", Json.str (match v.check with
          | .«exists» => "exists"
          | .creatable => "creatable"
          | .boxLocal => "box-local")),
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

def runSeatbelt (text : String) : IO UInt32 := do
  match Json.parse text >>= fromJson? (α := SeatbeltInput) with
  | .error e =>
    IO.eprintln s!"h5i-spec: bad seatbelt input: {e}"
    return 1
  | .ok inp =>
    let out := Json.arr <| (seatbeltFileRules inp).toArray.map fun r =>
      Json.mkObj [
        ("decision", Json.str (match r.decision with
          | .allow => "allow" | .deny => "deny")),
        ("ops", Json.arr (r.ops.toArray.map Json.str)),
        ("kind", Json.str (match r.kind with
          | .subpath => "subpath" | .literal => "literal"
          | .regex => "regex")),
        ("paths", Json.arr (r.paths.toArray.map Json.str)),
      ]
    IO.println out.compress
    return 0

/-- A node object `{id, kind, target?}` into one `nodes` entry. -/
def parseNode (j : Json) : Except String (NodeId × NodeKind) := do
  let id ← j.getObjValAs? Nat "id"
  let kind ← match (← j.getObjValAs? String "kind") with
    | "file" => pure NodeKind.file
    | "dir" => pure NodeKind.dir
    | "symlink" => pure (NodeKind.symlink (← j.getObjValAs? (List String) "target"))
    | other => throw s!"unknown node kind '{other}'"
  pure (id, kind)

def parseEntry (j : Json) : Except String Entry := do
  pure ⟨← j.getObjValAs? Nat "parent", ← j.getObjValAs? String "name",
        ← j.getObjValAs? Nat "child"⟩

def parseContent (j : Json) : Except String (NodeId × Nat) := do
  pure (← j.getObjValAs? Nat "id", ← j.getObjValAs? Nat "val")

/-- The measured world `{nodes, entries, content, root}` into an `FsState`.
Metadata is not needed by `validate`, so it is left empty. -/
def parseWorld (j : Json) : Except String FsState := do
  let nodes ← (← (← j.getObjVal? "nodes").getArr?).toList.mapM parseNode
  let entries ← (← (← j.getObjVal? "entries").getArr?).toList.mapM parseEntry
  let content ← (← (← j.getObjVal? "content").getArr?).toList.mapM parseContent
  pure { nodes, entries, content, metas := [], root := ← j.getObjValAs? Nat "root" }

/-- One validator case `{policy:{mayRead,mayWrite}, world, plan:{ro,rw}}` to the
`validate` verdict. Mounts are irrelevant to the authority subset check, so the
plan carries none here. -/
def evalValidateCase (j : Json) : Except String Bool := do
  let pj ← j.getObjVal? "policy"
  let pol : Policy := ⟨← pj.getObjValAs? (List Nat) "mayRead",
                       ← pj.getObjValAs? (List Nat) "mayWrite"⟩
  let world ← parseWorld (← j.getObjVal? "world")
  let plj ← j.getObjVal? "plan"
  let plan : EffectivePlan := ⟨← plj.getObjValAs? (List (List String)) "ro",
                              ← plj.getObjValAs? (List (List String)) "rw", []⟩
  pure (validate pol world plan)

def runValidate (text : String) : IO UInt32 := do
  match Json.parse text >>= (·.getArr?) with
  | .error e =>
    IO.eprintln s!"h5i-spec: bad validate input: {e}"
    return 1
  | .ok cases =>
    match cases.toList.mapM evalValidateCase with
    | .error e =>
      IO.eprintln s!"h5i-spec: bad validate case: {e}"
      return 1
    | .ok verdicts =>
      IO.println (Json.arr (verdicts.map Json.bool |>.toArray)).compress
      return 0

def main (args : List String) : IO UInt32 := do
  let stdin ← IO.getStdin
  let text ← stdin.readToEnd
  match args with
  | ["--predict"] => runPredict text
  | ["--interferes"] => runInterferes text
  | ["--seatbelt"] => runSeatbelt text
  | ["--validate"] => runValidate text
  | _ =>
    IO.eprintln
      "usage: h5i-spec --predict|--interferes|--seatbelt|--validate  (input on stdin)"
    return 2
