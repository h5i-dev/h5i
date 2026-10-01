import LedgerKernel
import H5iAppLib
/-!
# What the ledger should do

The file to review: what a write does, how much money the accounts hold,
and the invariants, on plain lists and natural numbers.
-/
open Aeneas Aeneas.Std ledger_kernel

namespace ledger_kernel.Spec

/-- The state as lists and numbers: the ledger row's three counters and the
accounts. -/
structure St where
  next : Nat
  deposited : Nat
  withdrawn : Nat
  accounts : List Account

def Snapshot.toSt (s : Snapshot) : St :=
  ⟨s.ledger.next_id.val, s.ledger.deposited.val, s.ledger.withdrawn.val, s.accounts.val⟩

def findAcc (s : St) (id : Nat) : Option Account :=
  s.accounts.find? (fun a => a.id.val = id)

/-- The money in all accounts together. -/
def total (s : St) : Nat :=
  (s.accounts.map (fun a => a.balance.val)).sum

/-- What a write does to the state. `H5iAppLib.upsert` replaces the row with the
same key, or appends it. -/
def applyWrite (s : St) : Write → St
  | .PutAccount a => { s with accounts := H5iAppLib.upsert (·.id) a s.accounts }
  | .SetLedger l => { s with next := l.next_id.val, deposited := l.deposited.val, withdrawn := l.withdrawn.val }

def applyAll (s : St) (ws : List Write) : St := ws.foldl applyWrite s

/-- Facts that hold in every state the ledger can reach. -/
structure Inv (s : St) : Prop where
  fresh : ∀ a ∈ s.accounts, a.id.val < s.next
  -- No money is created or lost.
  conserved : total s + s.withdrawn = s.deposited
  -- Total deposits fit in a u64, so every sum of balances does too.
  fits : s.deposited < 2 ^ 64

def init : St := ⟨0, 0, 0, []⟩

/-- The states reachable from an empty ledger by successful commands. -/
inductive Reachable : St → Prop
  | init : Reachable init
  | step {a s c ws r} : Reachable (Snapshot.toSt s) → transition a s c = .ok (.Ok (ws, r)) →
      Reachable (applyAll (Snapshot.toSt s) ws.val)

end ledger_kernel.Spec
