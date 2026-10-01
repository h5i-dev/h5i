import KeysKernel
import H5iAppLib
/-!
# The API-keys specification

`St` is the database as lists; `applyAll` commits a write set (`Apply.lean`
proves the extracted `apply` computes it). `resolveM` is the key a credential
acts for now, and `Effect` lists what each successful command writes.
-/
open Aeneas Aeneas.Std Result H5iAppLib keys_kernel

namespace Keys

h5i_derive_eq Perm Perm.Insts.CoreCmpPartialEqPerm.eq
h5i_derive_eq Action Action.Insts.CoreCmpPartialEqAction.eq

def permitsM (p : Perm) (a : Action) : Bool :=
  decide (p = .All ∨ (p = .Read ∧ a = .Read) ∨ (p = .Write ∧ a = .Write))

/-- Some permission in the list allows `a`. -/
def grantsM (ps : List Perm) (a : Action) : Bool := ps.any (permitsM · a)

/-! ## State -/

structure St where
  keys : List Key
  revoked : List (List U8)
  sessions : List Session
  next : U64

def toSt (s : Snapshot) : St := ⟨s.keys.val, s.revoked.val.map (·.val), s.sessions.val, s.next_session⟩

def applyW (st : St) : Write → St
  | .PutKey k => { st with keys := st.keys ++ [k] }
  | .DelKey sec => { st with keys := st.keys.filter (fun k => k.secret ≠ sec) }
  | .Revoked sec => { st with revoked := st.revoked ++ [sec.val] }
  | .PutSession x => { st with sessions := st.sessions ++ [x], next := core.num.U64.wrapping_add x.id 1#u64 }

def applyAll (st : St) (ws : alloc.vec.Vec Write) : St := ws.val.foldl applyW st

/-- The empty database. -/
def init : St := ⟨[], [], [], 0#u64⟩

/-! ## Credentials -/

def findKey (keys : List Key) (sec : alloc.vec.Vec U8) : Option Key := keys.find? (fun k => k.secret = sec)

/-- The key a credential acts for: a live key, or the live key of a session. -/
def resolveM (s : Snapshot) : Cred → Option Key
  | .Admin => none
  | .Key sec => findKey s.keys.val sec
  | .Session id => match s.sessions.val.find? (fun x => x.id = id) with
    | none => none
    | some x => findKey s.keys.val x.secret

/-- Before the fix: a session's copy of its key. -/
def resolvePreM (s : Snapshot) : Cred → Option Key
  | .Admin => none
  | .Key sec => findKey s.keys.val sec
  | .Session id => match s.sessions.val.find? (fun x => x.id = id) with
    | none => none
    | some x => some ⟨x.secret, x.perms⟩

def takenM (s : Snapshot) (sec : alloc.vec.Vec U8) : Bool :=
  (findKey s.keys.val sec).isSome || s.revoked.val.any (fun r => r = sec)

/-- What a successful command writes and replies, given how credentials resolve. -/
inductive Effect (resolve : Snapshot → Cred → Option Key) (s : Snapshot) :
    Cred → Command → List Write → Reply → Prop
  | issue {sec perms} : takenM s sec = false →
      Effect resolve s .Admin (.Issue sec perms) [.PutKey ⟨sec, perms⟩] .Done
  | revoke {sec} : Effect resolve s .Admin (.Revoke sec) [.DelKey sec, .Revoked sec] .Done
  | open_ {a k} : resolve s a = some k → s.next_session ≠ core.num.U64.MAX →
      Effect resolve s a .Open [.PutSession ⟨s.next_session, k.secret, k.perms⟩] (.Opened s.next_session)
  | act {a k act} : resolve s a = some k → grantsM k.perms.val act = true →
      Effect resolve s a (.Act act) [] (.Acted k.secret)

/-- No revoked secret belongs to a key. -/
def Inv (st : St) : Prop := ∀ r ∈ st.revoked, ∀ k ∈ st.keys, k.secret.val ≠ r

end Keys
