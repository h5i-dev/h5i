import CratesioKernel
import H5iAppLib
/-!
# What the crates.io kernel should do

This is the file to review. It states, over plain lists, who may make each
kind of write, what a write does to the state, which facts hold in every
reachable state, and the rules for deleting a crate.
-/
open Aeneas Aeneas.Std cratesio_kernel

namespace cratesio_kernel.Spec

/-- The registry as lists. -/
structure St where
  counter : Counter
  users : List User
  sessions : List Session
  tokens : List Token
  crates : List Krate
  versions : List Version
  owners : List Owner
  invites : List Invite
  deps : List Dep

def Snapshot.toSt (s : Snapshot) : St :=
  ⟨s.counter, s.users.val, s.sessions.val, s.tokens.val, s.crates.val, s.versions.val, s.owners.val,
    s.invites.val, s.deps.val⟩

/-! ## Lookups -/

def userOf (s : St) (u : U64) : Option User := s.users.find? (·.id = u)
def sessionOf (s : St) (i : U64) : Option Session := s.sessions.find? (·.id = i)
def tokenOf (s : St) (i : U64) : Option Token := s.tokens.find? (·.id = i)
def crateOf (s : St) (k : U64) : Option Krate := s.crates.find? (·.id = k)
def versionOf (s : St) (k n : U64) : Option Version := s.versions.find? (fun v => v.krate = k ∧ v.num = n)
def inviteOf (s : St) (k u : U64) : Option Invite := s.invites.find? (fun i => i.krate = k ∧ i.user = u)
def hasCrate (s : St) (k : U64) : Bool := s.crates.any (·.id = k)

/-- Locked with no end, or with an end still to come. -/
def lockedAt (u : User) (now : U64) : Bool := u.locked && (u.lock_until = 0#u64 || now < u.lock_until)

/-- The caller's account exists and is locked at the time of the request. -/
def lockedNow (s : St) (p : Principal) : Prop := ∃ u, userOf s p.user = some u ∧ lockedAt u p.now = true

/-- Not revoked and not expired. -/
def live (t : Token) (now : U64) : Bool := !t.revoked && (t.expires = 0#u64 || now < t.expires)

/-- Full for a user owner, Publish for a member of a team owner. -/
def rights (s : St) (k u : U64) (teams : List U64) : Rights :=
  if s.owners.any (fun o => decide (o.krate = k ∧ o.owner = u ∧ o.team = false)) then .Full
  else if s.owners.any (fun o => o.krate = k && o.team && decide (o.owner ∈ teams)) then .Publish
  else .None

/-! ## Signing in and token scopes -/

/-- The caller holds a session or a live token of their own, and their account
is not locked. `tok` is the token used, if any. -/
def SignedIn (s : St) (p : Principal) (u : User) (tok : Option Token) : Prop :=
  userOf s p.user = some u ∧ lockedAt u p.now = false ∧
    match p.via with
    | .Cookie sid => tok = none ∧ ∃ x, sessionOf s sid = some x ∧ x.user = p.user
    | .Token tid => ∃ t, tok = some t ∧ tokenOf s tid = some t ∧ t.user = p.user ∧ live t p.now
    | _ => False

/-- A token's endpoint scopes admit `e`. Legacy tokens admit everything. -/
def endpointOk (t : Token) : Endpoint → Bool
  | .Unscoped => t.legacy
  | .PublishNew => t.legacy || t.publish_new
  | .PublishUpdate => t.legacy || t.publish_update
  | .Yank => t.legacy || t.yank
  | .ChangeOwners => t.legacy || t.change_owners

/-- A token's crate scope admits the crate `k` (`none`: the endpoint names no crate). -/
def crateOk (t : Token) (k : Option U64) : Bool :=
  t.legacy || match t.krate, k with
    | none, _ => true
    | some c, some k => c = k
    | some _, none => false

/-- The endpoint and crate a token must be scoped for to make a write, or
`none` if no token may make it. -/
def needs (s : St) : Write → Option (Endpoint × Option U64)
  | .PutToken _ => some (.Unscoped, none)
  | .PutCrate k => some (.PublishNew, some k.id)
  | .PutVersion v =>
    if (versionOf s v.krate v.num).isSome then some (.Yank, some v.krate)
    else if hasCrate s v.krate then some (.PublishUpdate, some v.krate)
    else some (.PublishNew, some v.krate)
  | .PutOwner o =>
    if o.team then some (.ChangeOwners, some o.krate)
    else if hasCrate s o.krate then some (.Unscoped, none)
    else some (.PublishNew, some o.krate)
  | .DelOwner o => some (.ChangeOwners, some o.krate)
  | .PutInvite i => some (.ChangeOwners, some i.krate)
  | .DelInvite _ _ => some (.Unscoped, none)
  | .PutDep d => if hasCrate s d.krate then some (.PublishUpdate, some d.krate) else some (.PublishNew, some d.krate)
  | _ => none

/-! ## The policy -/

/-- May principal `p` make write `w` in state `s`? Judged against the state
before the command. -/
def allowed (s : St) (p : Principal) : Write → Prop
  | .PutUser x =>
    -- Signing up creates the caller's account, unlocked and unverified.
    (p.via = .GitHub ∧ userOf s p.user = none ∧ x = ⟨p.user, false, false, 0#u64, false⟩) ∨
    -- A user confirms their own email address.
    (∃ u, userOf s p.user = some u ∧ x = { u with verified := true }) ∨
    -- The operator changes an existing account's lock or admin flag.
    (p.via = .Operator ∧ ∃ u, userOf s x.id = some u ∧ x.verified = u.verified)
  | .PutSession x => p.via = .GitHub ∧ x.user = p.user ∧ x.id = s.counter.next_session
  | .PutToken t =>
    -- A new token of the caller's, or the caller revokes one of their own.
    t.user = p.user ∧ ((t.id = s.counter.next_token ∧ t.revoked = false) ∨
      ∃ o, tokenOf s t.id = some o ∧ o.user = p.user ∧ t = { o with revoked := true })
  | .PutCrate k => hasCrate s k.id = false ∧ k.created = p.now
  | .PutVersion v =>
    match versionOf s v.krate v.num with
    -- A new version by the caller: Publish rights, or the first one of a new crate.
    | none => v.publisher = p.user ∧ v.yanked = false ∧ (rights s v.krate p.user p.teams.val ≠ .None ∨ hasCrate s v.krate = false)
    -- A yank or unyank: only the flag changes, by Publish rights or an admin.
    | some o => v.publisher = o.publisher ∧
        (rights s v.krate p.user p.teams.val ≠ .None ∨ ∃ u, userOf s p.user = some u ∧ u.admin)
  | .PutOwner o =>
    -- A team the caller belongs to, added by a user owner.
    if o.team then rights s o.krate p.user p.teams.val = .Full ∧ o.owner ∈ p.teams.val
    -- The caller becomes a user owner by creating the crate or accepting a live invitation.
    else o.owner = p.user ∧ (hasCrate s o.krate = false ∨ ∃ i, inviteOf s o.krate p.user = some i ∧ p.now < i.expires)
  | .DelOwner o => rights s o.krate p.user p.teams.val = .Full
  | .PutInvite i => rights s i.krate p.user p.teams.val = .Full ∧ i.inviter = p.user
  | .DelInvite _ u => u = p.user
  | .PutDep d =>
    -- Part of publishing a new version of `d.krate`; the dependency is a known crate.
    (rights s d.krate p.user p.teams.val ≠ .None ∨ hasCrate s d.krate = false) ∧
      versionOf s d.krate d.num = none ∧ hasCrate s d.on
  | .DelCrate k => rights s k p.user p.teams.val = .Full
  | .SetCounter c =>
    (c.next_session.val = s.counter.next_session.val + 1 ∧ c.next_token = s.counter.next_token) ∨
    (c.next_token.val = s.counter.next_token.val + 1 ∧ c.next_session = s.counter.next_session)

/-! ## Deleting a crate -/

/-- Seconds since the crate was published (0 if the clock is behind). -/
def age (k : Krate) (now : U64) : Nat := now.val - k.created.val

/-- `max_downloads`: 1000 downloads per started month of 30 days. -/
def maxDownloads (age : Nat) : Nat := 1000 * ((age / 86400 + 29) / 30)

/-- delete.rs: within 72 hours always; later only with one owner (users and
teams) and few downloads. -/
def mayDelete (s : St) (k : Krate) (now downloads : U64) : Prop :=
  age k now ≤ 72 * 3600 ∨
    ((s.owners.filter (·.krate = k.id)).length ≤ 1 ∧ downloads.val ≤ maxDownloads (age k now))

/-- A version of another crate depends on `k`. -/
def hasReverseDep (s : St) (k : U64) : Bool := s.deps.any (fun d => decide (d.on = k ∧ d.krate ≠ k))

/-! ## Meaning of writes -/

open H5iAppLib in
/-- What a write does. `upsert` replaces the row with the same key, or appends it. -/
def applyWrite (s : St) : Write → St
  | .PutUser x => { s with users := upsert (·.id) x s.users }
  | .PutSession x => { s with sessions := upsert (·.id) x s.sessions }
  | .PutToken x => { s with tokens := upsert (·.id) x s.tokens }
  | .PutCrate x => { s with crates := upsert (·.id) x s.crates }
  | .PutVersion x => { s with versions := upsert (fun v => (v.krate, v.num)) x s.versions }
  | .PutOwner x => { s with owners := upsert (fun o => (o.krate, o.owner, o.team)) x s.owners }
  | .DelOwner x => { s with owners := s.owners.filter (fun o => (o.krate, o.owner, o.team) ≠ (x.krate, x.owner, x.team)) }
  | .PutInvite x => { s with invites := upsert (fun i => (i.krate, i.user)) x s.invites }
  | .DelInvite k u => { s with invites := s.invites.filter (fun i => (i.krate, i.user) ≠ (k, u)) }
  | .PutDep x => { s with deps := upsert (fun d => (d.krate, d.num, d.on)) x s.deps }
  | .DelCrate k => { s with
      crates := s.crates.filter (·.id ≠ k)
      versions := s.versions.filter (·.krate ≠ k)
      owners := s.owners.filter (·.krate ≠ k)
      invites := s.invites.filter (·.krate ≠ k)
      deps := s.deps.filter (·.krate ≠ k) }
  | .SetCounter c => { s with counter := c }

def applyAll (s : St) (ws : List Write) : St := ws.foldl applyWrite s

/-! ## Invariants -/

/-- Facts that hold in every reachable state. -/
structure Inv (s : St) : Prop where
  user_keys : (s.users.map (·.id)).Nodup
  session_keys : (s.sessions.map (·.id)).Nodup
  token_keys : (s.tokens.map (·.id)).Nodup
  crate_keys : (s.crates.map (·.id)).Nodup
  version_keys : (s.versions.map (fun v => (v.krate, v.num))).Nodup
  owner_keys : (s.owners.map (fun o => (o.krate, o.owner, o.team))).Nodup
  invite_keys : (s.invites.map (fun i => (i.krate, i.user))).Nodup
  dep_keys : (s.deps.map (fun d => (d.krate, d.num, d.on))).Nodup
  /-- New sessions and tokens never replace old ones. -/
  session_fresh : ∀ x ∈ s.sessions, x.id.val < s.counter.next_session.val
  token_fresh : ∀ t ∈ s.tokens, t.id.val < s.counter.next_token.val
  /-- Every crate has a user owner, who has Full rights. -/
  owned : ∀ k ∈ s.crates, ∃ o ∈ s.owners, o.krate = k.id ∧ o.team = false
  /-- User owners are registered users. -/
  owner_users : ∀ o ∈ s.owners, o.team = false → ∃ u ∈ s.users, u.id = o.owner
  /-- Versions, owners, invitations and dependencies belong to existing
  crates, so deleting a crate leaves nothing behind. -/
  version_crates : ∀ v ∈ s.versions, hasCrate s v.krate
  owner_crates : ∀ o ∈ s.owners, hasCrate s o.krate
  invite_crates : ∀ i ∈ s.invites, hasCrate s i.krate
  dep_crates : ∀ d ∈ s.deps, hasCrate s d.krate
  /-- Every dependency names an existing crate. -/
  dep_targets : ∀ d ∈ s.deps, hasCrate s d.on

def init : St := ⟨⟨0#u64, 0#u64⟩, [], [], [], [], [], [], [], []⟩

/-- The states reachable from an empty registry by successful commands of the
fixed kernel. -/
inductive Reachable : St → Prop
  | init : Reachable init
  | step {p s c ws r} : Reachable (Snapshot.toSt s) → transition p s c = .ok (.Ok (ws, r)) →
      Reachable (applyAll (Snapshot.toSt s) ws.val)

end cratesio_kernel.Spec
