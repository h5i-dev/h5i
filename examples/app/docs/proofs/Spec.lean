import DocsKernel
import H5iAppLib.Tables
/-!
# Specification of the example app

This is the file a reviewer reads. Everything is stated over plain lists and
the policy table below. Nothing here refers to the kernel's own helpers
(`can`, `role_of`, ...), so the theorems in `Theorems.lean` are not circular.
-/
open Aeneas Aeneas.Std

namespace docs_kernel

deriving instance DecidableEq for Role, Action, Status

end docs_kernel

namespace docs_kernel.Spec

/-! ## Policy (the requirement, typed in from the product spec) -/

def policy : Role → Action → Bool
  | .Owner, _ => true
  | .Editor, .Read => true
  | .Editor, .Write => true
  | .Viewer, .Read => true
  | _, _ => false

/-! ## State as lists -/

structure St where
  next : Nat
  projects : List Project
  members : List Member
  docs : List Document
  webhooks : List Webhook

def Snapshot.toSt (s : Snapshot) : St :=
  ⟨s.counter.next_id.val, s.projects.val, s.members.val, s.documents.val, s.webhooks.val⟩

def roleOf (ms : List Member) (p u : Nat) : Option Role :=
  (ms.find? (fun m => m.project.val = p ∧ m.user.val = u)).map (·.role)

/-- `u` may do `a` in project `p`. -/
def allowed (s : St) (u p : Nat) (a : Action) : Bool :=
  match roleOf s.members p u with
  | some r => policy r a
  | none => false

/-- The destination registered for a project, if any. -/
def webhookOf (hs : List Webhook) (p : Nat) : Option Nat :=
  (hs.find? (fun w => w.project.val = p)).map (·.dest.val)

def findDoc (ds : List Document) (id : Nat) : Option Document :=
  ds.find? (fun d => d.id.val = id)

def owners (ms : List Member) (p : Nat) : Nat :=
  (ms.filter (fun m => m.project.val = p ∧ m.role = .Owner)).length

/-! ## Meaning of a write set (what the database must store) -/

-- `H5iAppLib.upsert`: replace the row with the same key, or append.

def applyWrite (s : St) : Write → St
  | .PutProject p => { s with projects := H5iAppLib.upsert (fun q => (q.id.val, 0)) p s.projects }
  | .PutMember m => { s with members := H5iAppLib.upsert (fun n => (n.project.val, n.user.val)) m s.members }
  | .DelMember p u => { s with members := s.members.filter (fun n => ¬(n.project = p ∧ n.user = u)) }
  | .PutDocument d => { s with docs := H5iAppLib.upsert (fun e => (e.id.val, 0)) d s.docs }
  | .DelDocument i => { s with docs := s.docs.filter (fun e => e.id ≠ i) }
  | .SetCounter c => { s with next := c.next_id.val }
  | .PutWebhook w => { s with webhooks := H5iAppLib.upsert (fun h => (h.project.val, 0)) w s.webhooks }
  | .DelWebhook p => { s with webhooks := s.webhooks.filter (fun h => h.project ≠ p) }
  | .Emit _ => s

def applyAll (s : St) (ws : List Write) : St := ws.foldl applyWrite s

/-! ## Invariants -/

structure Inv (s : St) : Prop where
  /-- Every project has an owner. -/
  owned : ∀ p ∈ s.projects, 0 < owners s.members p.id.val
  /-- Memberships and documents belong to existing projects. -/
  member_proj : ∀ m ∈ s.members, ∃ p ∈ s.projects, p.id = m.project
  doc_proj : ∀ d ∈ s.docs, ∃ p ∈ s.projects, p.id = d.project
  /-- Keys are unique. -/
  proj_keys : (s.projects.map (·.id)).Nodup
  member_keys : (s.members.map (fun m => (m.project, m.user))).Nodup
  doc_keys : (s.docs.map (·.id)).Nodup
  /-- Ids come from the counter. -/
  proj_fresh : ∀ p ∈ s.projects, p.id.val < s.next
  doc_fresh : ∀ d ∈ s.docs, d.id.val < s.next
  /-- Four-eyes rule: approved and published documents were approved by
  someone other than their author. -/
  four_eyes : ∀ d ∈ s.docs, d.status = .Approved ∨ d.status = .Published →
    ∃ a, d.approver = some a ∧ a ≠ d.author
  /-- A document names an approver exactly when it is approved or published. -/
  approver_iff : ∀ d ∈ s.docs, d.approver.isSome ↔ (d.status = .Approved ∨ d.status = .Published)
  /-- Webhooks belong to existing projects, at most one per project. -/
  hook_proj : ∀ w ∈ s.webhooks, ∃ p ∈ s.projects, p.id = w.project
  hook_keys : (s.webhooks.map (·.project)).Nodup

def init : St := ⟨0, [], [], [], []⟩

/-- States the database can be in: built from `init` by committed transitions. -/
inductive Reachable : St → Prop
  | init : Reachable init
  | step {s : Snapshot} {a c ws r} :
      Reachable (Snapshot.toSt s) →
      transition a s c = .ok (.Ok (ws, r)) →
      Reachable (applyAll (Snapshot.toSt s) ws.val)

/-! ## Authorization, stated on effects -/

/-- Allowed status changes. Editing sends a document back to draft, except
once published. -/
def statusStep : Status → Status → Bool
  | .Published, .Draft => false
  | _, .Draft => true
  | .Draft, .InReview => true
  | .InReview, .Approved => true
  | .Approved, .Published => true
  | _, _ => false

/-- The permission a single write needs, judged against the state before the
whole write set. -/
def writeAllowed (s : St) (u : Nat) : Write → Prop
  | .PutProject p => p.id.val = s.next
  | .PutMember m =>
      -- creating a fresh project, or managing an existing one
      (m.project.val = s.next ∧ m.user.val = u ∧ m.role = .Owner) ∨
      allowed s u m.project.val .Manage
  | .DelMember p _ => allowed s u p.val .Manage
  | .PutDocument d =>
      match findDoc s.docs d.id.val with
      | none =>
          d.author.val = u ∧ d.status = .Draft ∧ d.approver = none ∧
          allowed s u d.project.val .Write
      | some old =>
          old.project = d.project ∧ old.author = d.author ∧
          statusStep old.status d.status ∧
          (if d.approver ≠ old.approver ∧ d.approver ≠ none
           then d.approver.map (·.val) = some u ∧ d.author.val ≠ u ∧
             allowed s u d.project.val .Approve
           else allowed s u d.project.val .Write)
  | .DelDocument i =>
      ∃ d, findDoc s.docs i.val = some d ∧ allowed s u d.project.val .Manage
  | .SetCounter c => c.next_id.val = s.next + 1
  | .PutWebhook w => allowed s u w.project.val .Manage
  | .DelWebhook p => allowed s u p.val .Manage
  | .Emit e =>
      -- The destination is the one the project registered, and only a writer
      -- publishing an approved document of that project triggers it.
      webhookOf s.webhooks e.project.val = some e.dest.val ∧
      allowed s u e.project.val .Write ∧
      ∃ d, findDoc s.docs e.doc.val = some d ∧ d.project = e.project ∧ d.status = .Approved

/-- What a reply may reveal: only documents the caller can read. -/
def replyAllowed (s : St) (u : Nat) : Reply → Prop
  | .Doc d => d ∈ s.docs ∧ allowed s u d.project.val .Read
  | .Docs ds => ∀ d ∈ ds.val, d ∈ s.docs ∧ allowed s u d.project.val .Read
  | _ => True

/-! ## Noninterference -/

/-- Everything user `u` is entitled to see: the counter, and the memberships
and documents of projects `u` belongs to. The counter is a declared leak:
ids reveal how many objects the tenant has created. -/
def view (s : St) (u : Nat) : Nat × List Member × List Document × List Webhook :=
  let mine p := (roleOf s.members p u).isSome
  (s.next, s.members.filter (fun m => mine m.project.val), s.docs.filter (fun d => mine d.project.val),
    s.webhooks.filter (fun w => mine w.project.val))

end docs_kernel.Spec
