import InboxKernel
import H5iAppLib
/-!
# What the inbox should do

The file to review: what a user may see (`view`), which rows their writes
may touch, what a write does, and the invariants, on plain lists.
-/
open Aeneas Aeneas.Std inbox_kernel

namespace inbox_kernel.Spec

/-- The state as lists: the messages and the blocks. -/
structure St where
  msgs : List Message
  blocks : List Block

def Snapshot.toSt (s : Snapshot) : St :=
  ⟨s.messages.val, s.blocks.val⟩

/-- `u` sent or received `m`. -/
def involves (u : Nat) (m : Message) : Bool :=
  m.sender.val = u || m.recipient.val = u

/-- What `u` may learn: the messages `u` sent or received, and who blocks `u`. -/
def view (s : St) (u : Nat) : List Message × List Block :=
  (s.msgs.filter (involves u), s.blocks.filter (fun b => b.sender.val = u))

/-- The rows `u` must not change: everyone else's messages and blocks. -/
def others (s : St) (u : Nat) : List Message × List Block :=
  (s.msgs.filter (fun m => !involves u m), s.blocks.filter (fun b => b.owner.val ≠ u))

/-- A write by `u` only touches rows that belong to `u`. -/
def touches (u : Nat) : Write → Prop
  | .PutMessage m => involves u m
  | .PutBlock b => b.owner.val = u
  | .DelBlock b => b.owner.val = u

def msgKey (m : Message) : U64 × U64 × U64 := (m.sender, m.recipient, m.seq)
def blockKey (b : Block) : U64 × U64 := (b.owner, b.sender)

/-- What a write does to the state. `H5iAppLib.upsert` replaces the row with the
same key, or appends it. -/
def applyWrite (s : St) : Write → St
  | .PutMessage m => { s with msgs := H5iAppLib.upsert msgKey m s.msgs }
  | .PutBlock b => { s with blocks := H5iAppLib.upsert blockKey b s.blocks }
  | .DelBlock b => { s with blocks := s.blocks.filter (fun x => blockKey x ≠ blockKey b) }

def applyAll (s : St) (ws : List Write) : St := ws.foldl applyWrite s

/-- A message is non-empty and at most 1000 bytes. -/
def textOk (t : List U8) : Prop :=
  0 < t.length ∧ t.length ≤ 1000

/-- Facts that hold in every state the inbox can reach. -/
structure Inv (s : St) : Prop where
  msg_keys : (s.msgs.map msgKey).Nodup
  texts : ∀ m ∈ s.msgs, textOk m.text.val
  block_keys : (s.blocks.map blockKey).Nodup

/-- `m'` is `m` later on: the same text, and the read and deleted flags only
ever turn on. -/
def Later (m m' : Message) : Prop :=
  m'.text = m.text ∧ (m.read → m'.read) ∧ (m.sender_deleted → m'.sender_deleted) ∧
    (m.recipient_deleted → m'.recipient_deleted)

def init : St := ⟨[], []⟩

/-- The states reachable from an empty inbox by successful commands. -/
inductive Reachable : St → Prop
  | init : Reachable init
  | step {a s c ws r} : Reachable (Snapshot.toSt s) → transition a s c = .ok (.Ok (ws, r)) →
      Reachable (applyAll (Snapshot.toSt s) ws.val)

end inbox_kernel.Spec
