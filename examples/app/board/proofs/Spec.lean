import BoardKernel
import H5iAppLib
/-!
# What the bulletin board should do

The file to review: the permission policy, what a write does, and the
invariants, on plain lists and natural numbers.
-/
open Aeneas Aeneas.Std board_kernel

namespace board_kernel.Spec

/-- The state as lists: the next post id, the posts and the moderators. -/
structure St where
  next : Nat
  posts : List Post
  mods : List Moderator

def Snapshot.toSt (s : Snapshot) : St :=
  ⟨s.counter.next_id.val, s.posts.val, s.moderators.val⟩

def findPost (s : St) (id : Nat) : Option Post :=
  s.posts.find? (fun p => p.id.val = id)

def isMod (s : St) (u : Nat) : Prop :=
  ∃ m ∈ s.mods, m.user.val = u

/-- A post is non-empty and at most 280 bytes. -/
def textOk (t : List U8) : Prop :=
  0 < t.length ∧ t.length ≤ 280

/-- The policy: may user `u` make write `w` in state `s`? -/
def allowed (s : St) (u : Nat) : Write → Prop
  | .PutPost p =>
    -- A new post by `u`, or an edit of `u`'s own post; the author never changes.
    p.author.val = u ∧ textOk p.text.val ∧
      (p.id.val = s.next ∨ ∃ q, findPost s p.id.val = some q ∧ q.author.val = u)
  | .DelPost id =>
    -- The author or a moderator deletes an existing post.
    ∃ q, findPost s id.val = some q ∧ (q.author.val = u ∨ isMod s u)
  | .PutModerator m =>
    -- A moderator appoints anyone; with no moderators, a user appoints themselves.
    isMod s u ∨ (s.mods = [] ∧ m.user.val = u)
  | .DelModerator t =>
    -- A moderator removes a moderator, and another one remains.
    isMod s u ∧ ∃ m ∈ s.mods, m.user ≠ t
  | .SetCounter c => c.next_id.val = s.next + 1

/-- What a write does to the state. `H5iAppLib.upsert` replaces the row with the
same key, or appends it. -/
def applyWrite (s : St) : Write → St
  | .PutPost p => { s with posts := H5iAppLib.upsert (·.id) p s.posts }
  | .DelPost id => { s with posts := s.posts.filter (fun p => p.id ≠ id) }
  | .PutModerator m => { s with mods := H5iAppLib.upsert (·.user) m s.mods }
  | .DelModerator u => { s with mods := s.mods.filter (fun m => m.user ≠ u) }
  | .SetCounter c => { s with next := c.next_id.val }

def applyAll (s : St) (ws : List Write) : St := ws.foldl applyWrite s

/-- Facts that hold in every state the board can reach. -/
structure Inv (s : St) : Prop where
  post_keys : (s.posts.map (·.id)).Nodup
  fresh : ∀ p ∈ s.posts, p.id.val < s.next
  texts : ∀ p ∈ s.posts, textOk p.text.val
  mod_keys : (s.mods.map (·.user)).Nodup

def init : St := ⟨0, [], []⟩

/-- The states reachable from an empty board by successful commands. -/
inductive Reachable : St → Prop
  | init : Reachable init
  | step {a s c ws r} : Reachable (Snapshot.toSt s) → transition a s c = .ok (.Ok (ws, r)) →
      Reachable (applyAll (Snapshot.toSt s) ws.val)

end board_kernel.Spec
