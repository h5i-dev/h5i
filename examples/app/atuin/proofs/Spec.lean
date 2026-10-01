import AtuinKernel
import H5iAppLib
/-!
# Specification of the Atuin port

The file to review, over plain lists; it does not use the kernel's helpers.
-/
open Aeneas Aeneas.Std atuin_kernel

namespace atuin_kernel.Spec

structure St where
  next : Nat
  openReg : Bool
  maxSize : Nat
  users : List User
  sessions : List Session
  records : List Record

def Snapshot.toSt (s : Snapshot) : St :=
  ⟨s.counter.next_id.val, s.settings.open_registration, s.settings.max_record_size.val,
    s.users.val, s.sessions.val, s.records.val⟩

/-- The account a principal speaks for, if it still exists. -/
def signedIn (s : St) : Principal → Option Nat
  | .User id => if s.users.any (fun u => u.id = id) then some id.val else none
  | .Anonymous => none

/-- The one user whose data a write touches (`none` for the id counter). -/
def owner : Write → Option Nat
  | .PutUser u => some u.id.val
  | .DelUser id => some id.val
  | .PutSession x => some x.user.val
  | .DelSession u => some u.val
  | .PutRecord r => some r.user.val
  | .DelRecordsOf u => some u.val
  | .SetCounter _ => none

/-- Isolation: a write touches only the caller's own account, or the account
being created (the next id). -/
def isolated (s : St) (a : Principal) (w : Write) : Prop :=
  ∀ o, owner w = some o → signedIn s a = some o ∨ o = s.next

/-- Replies contain only the caller's own records. -/
def replyAllowed (s : St) (a : Principal) : Reply → Prop
  | .Records rs => ∀ r ∈ rs.val, r ∈ s.records ∧ signedIn s a = some r.user.val
  | .Status ks => ∀ k ∈ ks.val, ∃ r ∈ s.records, signedIn s a = some r.user.val ∧ k = (r.host, r.tag, r.idx)
  | _ => True

/-! ## Meaning of a write set -/

def rkey (r : Record) : U64 × U64 × U64 × U64 := (r.user, r.host, r.tag, r.idx)

def applyWrite (s : St) : Write → St
  | .PutUser u => { s with users := H5iAppLib.upsert (·.id) u s.users }
  | .DelUser id => { s with users := s.users.filter (fun x => x.id ≠ id) }
  | .PutSession x => { s with sessions := H5iAppLib.upsert (·.user) x s.sessions }
  | .DelSession u => { s with sessions := s.sessions.filter (fun y => y.user ≠ u) }
  | .PutRecord r => { s with records := H5iAppLib.upsert rkey r s.records }
  | .DelRecordsOf u => { s with records := s.records.filter (fun y => y.user ≠ u) }
  | .SetCounter c => { s with next := c.next_id.val }

def applyAll (s : St) (ws : List Write) : St := ws.foldl applyWrite s

structure Inv (s : St) : Prop where
  /-- Deleting an account leaves nothing behind: every session and record
  belongs to an existing user. -/
  sessions_owned : ∀ x ∈ s.sessions, ∃ u ∈ s.users, u.id = x.user
  records_owned : ∀ r ∈ s.records, ∃ u ∈ s.users, u.id = r.user
  user_keys : (s.users.map (·.id)).Nodup
  usernames : (s.users.map (·.username.val)).Nodup
  session_keys : (s.sessions.map (·.user)).Nodup
  record_keys : (s.records.map (fun r => (r.user, r.host, r.tag, r.idx))).Nodup
  fresh : ∀ u ∈ s.users, u.id.val < s.next
  /-- Stored records respect the size cap (0 means no cap). -/
  sized : s.maxSize ≠ 0 → ∀ r ∈ s.records, r.data.length ≤ s.maxSize

end atuin_kernel.Spec
