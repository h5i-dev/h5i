import DocsKernel
/-!
Runs the extracted kernel on test cases from stdin, one per line, for
differential testing against the Rust kernel (`scripts/app/difftest.sh`).
Case and output encodings are space-separated numbers; see
`examples/app/docs/difftest/src/lib.rs`, which must stay in sync.
-/
open Aeneas Aeneas.Std docs_kernel

namespace DiffTest

abbrev P := StateT (List Nat) Option

def nat : P Nat := do
  match (← get) with
  | x :: xs => set xs; pure x
  | [] => failure

def u64 : P U64 := do
  let x ← nat
  if h : x < 2 ^ UScalarTy.U64.numBits then pure (UScalar.ofNatCore x h) else failure

def u8 : P U8 := do
  let x ← nat
  if h : x < 2 ^ UScalarTy.U8.numBits then pure (UScalar.ofNatCore x h) else failure

def vec {α : Type} (p : P α) : P (alloc.vec.Vec α) := do
  let n ← nat
  let l ← (List.range n).mapM (fun _ => p)
  if h : l.length ≤ Usize.max then pure (alloc.vec.Vec.from l h) else failure

def role : P Role := do
  match (← nat) with
  | 0 => pure .Viewer | 1 => pure .Editor | 2 => pure .Owner | _ => failure

def status : P Status := do
  match (← nat) with
  | 0 => pure .Draft | 1 => pure .InReview | 2 => pure .Approved | 3 => pure .Published
  | _ => failure

def opt (p : P α) : P (Option α) := do
  match (← nat) with
  | 0 => pure none | 1 => some <$> p | _ => failure

def project : P Project := do pure { id := ← u64, «name» := ← vec u8 }

def member : P Member := do pure { project := ← u64, user := ← u64, role := ← role }

def document : P Document := do
  pure { id := ← u64, project := ← u64, author := ← u64, title := ← vec u8, body := ← vec u8,
         status := ← status, approver := ← opt u64, version := ← u64 }

def webhook : P Webhook := do pure { project := ← u64, dest := ← u64 }

def snapshot : P Snapshot := do
  pure { counter := { next_id := ← u64 }, projects := ← vec project, members := ← vec member,
         documents := ← vec document, webhooks := ← vec webhook }

def command : P Command := do
  match (← nat) with
  | 0 => return .CreateProject (← vec u8)
  | 1 => return .SetMember (← u64) (← u64) (← role)
  | 2 => return .RemoveMember (← u64) (← u64)
  | 3 => return .CreateDocument (← u64) (← vec u8) (← vec u8)
  | 4 => return .EditDocument (← u64) (← vec u8) (← u64)
  | 5 => return .Submit (← u64)
  | 6 => return .Approve (← u64)
  | 7 => return .Publish (← u64)
  | 8 => return .DeleteDocument (← u64)
  | 9 => return .GetDocument (← u64)
  | 10 => return .ListDocuments (← u64)
  | 11 => return .SetWebhook (← u64) (← opt u64)
  | _ => failure

def case : P (Principal × Snapshot × Command) := do
  let a : Principal := { org := ← u64, user := ← u64 }
  let s ← snapshot
  let c ← command
  pure (a, s, c)

/-! Output encoding -/

def eBytes (v : alloc.vec.Vec U8) : List Nat := v.val.length :: v.val.map (·.val)

def eRole : Role → Nat | .Viewer => 0 | .Editor => 1 | .Owner => 2

def eStatus : Status → Nat | .Draft => 0 | .InReview => 1 | .Approved => 2 | .Published => 3

def eProject (p : Project) : List Nat := p.id.val :: eBytes p.name

def eMember (m : Member) : List Nat := [m.project.val, m.user.val, eRole m.role]

def eDoc (d : Document) : List Nat :=
  [d.id.val, d.project.val, d.author.val] ++ eBytes d.title ++ eBytes d.body ++
  [eStatus d.status] ++ (match d.approver with | none => [0] | some a => [1, a.val]) ++ [d.version.val]

def eList {α : Type} (f : α → List Nat) (l : List α) : List Nat := l.length :: (l.map f).flatten

def eHook (w : Webhook) : List Nat := [w.project.val, w.dest.val]

def eSnap (s : Snapshot) : List Nat :=
  s.counter.next_id.val :: eList eProject s.projects.val ++ eList eMember s.members.val ++
  eList eDoc s.documents.val ++ eList eHook s.webhooks.val

def eWrite : Write → List Nat
  | .PutProject p => 0 :: eProject p
  | .PutMember m => 1 :: eMember m
  | .DelMember p u => [2, p.val, u.val]
  | .PutDocument d => 3 :: eDoc d
  | .DelDocument i => [4, i.val]
  | .SetCounter c => [5, c.next_id.val]
  | .PutWebhook w => 6 :: eHook w
  | .DelWebhook p => [7, p.val]
  | .Emit e => [8, e.dest.val, e.project.val, e.doc.val, e.version.val]

def eReply : Reply → List Nat
  | .Created i => [0, i.val]
  | .Done => [1]
  | .Version v => [2, v.val]
  | .Doc d => 3 :: eDoc d
  | .Docs ds => 4 :: eList eDoc ds.val

def eError : docs_kernel.Error → Nat
  | .NotFound => 0 | .Forbidden => 1 | .Conflict => 2 | .BadState => 3
  | .LastOwner => 4 | .SelfApproval => 5 | .Overflow => 6

def str (l : List Nat) : String := " ".intercalate (l.map toString)

def runLine (line : String) : String :=
  let toks := ((line.splitOn " ").filter (· ≠ "")).filterMap String.toNat?
  match case.run toks with
  | some ((a, s, c), []) =>
    match (transition a s c).match with
    | .ok (core.result.Result.Err e) => s!"ERR {eError e}"
    | .ok (core.result.Result.Ok (ws, r)) =>
      match (apply s ws).match with
      | .ok s' => s!"OK {str (eList eWrite ws.val)} | {str (eReply r)} | {str (eSnap s')}"
      | _ => "APPLYFAIL"
    | _ => "FAIL"
  | _ => "PARSE"

end DiffTest

def main : IO Unit := do
  let stdin ← IO.getStdin
  let stdout ← IO.getStdout
  repeat
    let line ← stdin.getLine
    if line.isEmpty then break
    stdout.putStrLn (DiffTest.runLine line.trimAsciiEnd.toString)
  stdout.flush
