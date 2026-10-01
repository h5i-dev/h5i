import KellnrKernel
/-!
# Kellnr registry authorization: the spec

Written from Kellnr's documented rules, over plain lists. Nothing here uses
the kernel's helpers, so the theorems are not circular.
-/
open Aeneas Aeneas.Std

namespace kellnr_kernel

deriving instance DecidableEq for Login

end kellnr_kernel

namespace kellnr_kernel.Spec

structure St where
  users : List User
  crates : List Krate
  versions : List Version
  owners : List Pair
  crateUsers : List Pair
  crateGroups : List Pair
  groupMembers : List Pair
  allowOwnerless : Bool
  newCratesRestricted : Bool

def Snapshot.toSt (s : Snapshot) : St :=
  ⟨s.users.val, s.crates.val, s.versions.val, s.owners.val, s.crate_users.val,
    s.crate_groups.val, s.group_members.val, s.settings.allow_ownerless_crates,
    s.settings.new_crates_restricted⟩

/-! ## Facts read from the database -/

def userOf (s : St) (u : Nat) : Option User := s.users.find? (·.id.val = u)

def isAdmin (s : St) (u : Nat) : Bool := (userOf s u).any (·.is_admin)

/-- The user's read-only flag as stored, whatever the login path. -/
def isReadOnly (s : St) (u : Nat) : Bool := (userOf s u).any (·.is_read_only)

def hasPair (l : List Pair) (a b : Nat) : Bool := l.any (fun x => x.a.val = a ∧ x.b.val = b)

def isOwner (s : St) (k u : Nat) : Bool := hasPair s.owners k u

def isCrateUser (s : St) (k u : Nat) : Bool := hasPair s.crateUsers k u

/-- `u` is in a group granted on crate `k`. -/
def inGrantedGroup (s : St) (k u : Nat) : Bool :=
  s.crateGroups.any (fun g => g.a.val = k ∧ hasPair s.groupMembers g.b.val u)

def crateExists (s : St) (k : Nat) : Bool := s.crates.any (·.id.val = k)

def restricted (s : St) (k : Nat) : Bool := (s.crates.find? (·.id.val = k)).any (·.restricted)

def ownerCount (s : St) (k : Nat) : Nat := (s.owners.filter (·.a.val = k)).length

/-! ## Policy -/

/-- Read-only users may not change anything, unless they are admins. -/
def mayModify (s : St) (u : Nat) : Bool := isAdmin s u || !isReadOnly s u

/-- Who may make a single write, judged on the state before the command.
ACL and yank changes need an admin or an owner of the crate; publishing a new
crate makes the publisher its first owner. -/
def writeAllowed (s : St) (u : Nat) : Write → Prop
  | .AddOwner x => isAdmin s u ∨ isOwner s x.a.val u ∨ (¬crateExists s x.a.val ∧ x.b.val = u)
  | .DelOwner x => isAdmin s u ∨ isOwner s x.a.val u
  | .AddCrateUser x | .DelCrateUser x => isAdmin s u ∨ isOwner s x.a.val u
  | .AddCrateGroup x | .DelCrateGroup x => isAdmin s u ∨ isOwner s x.a.val u
  | .SetYanked v => isAdmin s u ∨ isOwner s v.krate.val u
  | .AddVersion v => isAdmin s u ∨ isOwner s v.krate.val u ∨ ¬crateExists s v.krate.val
  | .AddCrate k => ¬crateExists s k.id.val

/-- A restricted crate is served only to a token of an admin, owner, crate
user or member of a granted group. -/
def downloadAllowed (s : St) (p : Principal) (k : Nat) : Prop :=
  restricted s k → p.login = .Token ∧
    (isAdmin s p.user.val ∨ isOwner s k p.user.val ∨ isCrateUser s k p.user.val ∨
      inGrantedGroup s k p.user.val)

/-! ## Committing writes -/

/-- Add a row unless the pair is already there. -/
def putPair (l : List Pair) (x : Pair) : List Pair :=
  if hasPair l x.a.val x.b.val then l else l ++ [x]

/-- Remove every row with this pair. -/
def delPair (l : List Pair) (x : Pair) : List Pair :=
  l.filter fun o => ¬(o.a.val = x.a.val ∧ o.b.val = x.b.val)

/-- Replace the first row for the same crate and version. -/
def setYanked : List Version → Version → List Version
  | [], _ => []
  | v :: vs, x => if v.krate.val = x.krate.val ∧ v.vers.val = x.vers.val then x :: vs else v :: setYanked vs x

def applyWrite (s : St) : Write → St
  | .AddOwner x => { s with owners := putPair s.owners x }
  | .DelOwner x => { s with owners := delPair s.owners x }
  | .AddCrateUser x => { s with crateUsers := putPair s.crateUsers x }
  | .DelCrateUser x => { s with crateUsers := delPair s.crateUsers x }
  | .AddCrateGroup x => { s with crateGroups := putPair s.crateGroups x }
  | .DelCrateGroup x => { s with crateGroups := delPair s.crateGroups x }
  | .SetYanked v => { s with versions := setYanked s.versions v }
  | .AddCrate k => { s with crates := s.crates ++ [k] }
  | .AddVersion v => { s with versions := s.versions ++ [v] }

def applyAll (s : St) (ws : List Write) : St := ws.foldl applyWrite s

/-- The states reachable from an empty registry. Accounts, group members and
settings are managed outside the kernel, so any step may change them. -/
inductive Reachable : St → Prop
  | init (users : List User) (members : List Pair) (ownerless restricted : Bool) :
      Reachable ⟨users, [], [], [], [], [], members, ownerless, restricted⟩
  | step {p s c ws r} : Reachable (Snapshot.toSt s) → transition p s c = .ok (.Ok (ws, r)) →
      Reachable (applyAll (Snapshot.toSt s) ws.val)
  | admin {s} (users : List User) (members : List Pair) (ownerless restricted : Bool) :
      Reachable s → Reachable ⟨users, s.crates, s.versions, s.owners, s.crateUsers, s.crateGroups,
        members, ownerless, restricted⟩

end kellnr_kernel.Spec
