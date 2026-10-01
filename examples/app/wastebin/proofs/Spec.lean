import WastebinKernel
import H5iAppLib
/-!
# What Wastebin should do

The file to review: the write policy, what a write does, the invariants and
the issue #190 property, over lists and naturals.
-/
open Aeneas Aeneas.Std wastebin_kernel

namespace wastebin_kernel.Spec

/-- The state as lists: the next paste id, the last uid handed out, the pastes. -/
structure St where
  next : Nat
  lastUid : Nat
  pastes : List Paste

def Snapshot.toSt (s : Snapshot) : St :=
  ⟨s.counter.next_id.val, s.counter.last_uid.val, s.pastes.val⟩

def findSlug (s : St) (k : Nat) : Option Paste :=
  s.pastes.find? (fun p => p.slug.val = k)

/-- Wastebin's `expires < datetime('now')`; `now` comes from the shell. -/
def isExpired (now : Nat) (p : Paste) : Bool :=
  match p.expires with
  | some t => decide (t.val < now)
  | none => false

/-- The password check passes: no password, or the right fingerprint. -/
def Unlocked (p : Paste) (key : Option U64) : Prop :=
  p.lock = none ∨ p.lock = key

/-- A read of `p` shows `v`: its text and expiry, whether it burned, and
whether the caller may delete it. Nothing else. -/
def Shows (a : Principal) (p : Paste) (v : Shown) : Prop :=
  v.text = p.text ∧ v.expires = p.expires ∧ v.burned = p.burn ∧ (v.owned = true ↔ p.owner ∈ a.uids.val)

/-- The policy: may this request make write `w` in state `s`? -/
def allowed (s : St) (a : Principal) : Write → Prop
  | .PutPaste p =>
    -- A new paste under the next id and the request's unused random slug, owned
    -- by the caller's first uid, or by a new uid if the caller has none.
    p.id.val = s.next ∧ p.slug = a.fresh ∧ findSlug s a.fresh.val = none ∧
      (a.uids.val.head? = some p.owner ∨ (a.uids.val = [] ∧ p.owner.val = s.lastUid + 1))
  | .DelPaste id =>
    -- The owner deletes it, it has expired, or it is burned by a read.
    ∃ p ∈ s.pastes, p.id = id ∧ (p.owner ∈ a.uids.val ∨ isExpired a.now.val p ∨ p.burn)
  | .SetCounter c =>
    c.next_id.val = s.next + 1 ∧ (c.last_uid.val = s.lastUid ∨ c.last_uid.val = s.lastUid + 1)

/-- What a write does. `H5iAppLib.upsert` replaces the row with the same key,
or appends it. -/
def applyWrite (s : St) : Write → St
  | .PutPaste p => { s with pastes := H5iAppLib.upsert (·.id) p s.pastes }
  | .DelPaste id => { s with pastes := s.pastes.filter (fun p => p.id ≠ id) }
  | .SetCounter c => { s with next := c.next_id.val, lastUid := c.last_uid.val }

def applyAll (s : St) (ws : List Write) : St := ws.foldl applyWrite s

/-- Facts that hold in every reachable state. -/
structure Inv (s : St) : Prop where
  ids : (s.pastes.map (·.id)).Nodup
  slugs : (s.pastes.map (·.slug)).Nodup
  fresh : ∀ p ∈ s.pastes, p.id.val < s.next

def init : St := ⟨0, 0, []⟩

/-- A kernel variant: `transition` or `transition_pre190`. -/
abbrev Kernel := Principal → Snapshot → Command →
  Result (core.result.Result (alloc.vec.Vec Write × Reply) Error)

/-- The states reachable from `s` by successful commands of kernel `T`. -/
inductive Steps (T : Kernel) (s : St) : St → Prop
  | refl : Steps T s s
  | step {a s' c ws r} : Steps T s (Snapshot.toSt s') → T a s' c = .ok (.Ok (ws, r)) →
      Steps T s (applyAll (Snapshot.toSt s') ws.val)

abbrev Reachable (T : Kernel) (s : St) : Prop := Steps T init s

/-- Issue #190: in a reachable state, a request for the paste page without
the confirmation field, which is what a link preview sends, never removes a
paste that has not expired and never shows a burn-after-reading paste. -/
def PreviewSafe (T : Kernel) : Prop :=
  ∀ a s k key ws r, Reachable T (Snapshot.toSt s) → T a s (.View k false key) = .ok (.Ok (ws, r)) →
    (∀ p ∈ (Snapshot.toSt s).pastes, isExpired a.now.val p = false →
      p ∈ (applyAll (Snapshot.toSt s) ws.val).pastes) ∧
    (∀ v, r = .Shown v → v.burned = false)

end wastebin_kernel.Spec
