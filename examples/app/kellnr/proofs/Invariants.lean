import Theorems
/-!
# Owner rows in reachable states

`apply` adds an owner row only if absent, so owner rows stay unique. Hence,
while ownerless crates are disallowed, removing an owner never leaves none.
-/
open Aeneas Aeneas.Std Result kellnr_kernel kellnr_kernel.Spec kellnr_kernel.Theorems

namespace kellnr_kernel.Invariants

/-- Each (crate, user) owner pair is stored once. -/
def OwnersUnique (s : St) : Prop := (s.owners.map fun o => (o.a.val, o.b.val)).Nodup

theorem putPair_nodup {l : List Pair} (x : Pair) (h : (l.map fun o => (o.a.val, o.b.val)).Nodup) :
    ((putPair l x).map fun o => (o.a.val, o.b.val)).Nodup := by
  unfold putPair
  split
  · exact h
  · rename_i hn
    simp only [hasPair, List.any_eq_true, decide_eq_true_eq, not_exists, not_and] at hn
    rw [List.map_append, List.nodup_append]
    refine ⟨h, by simp, ?_⟩
    intro a ha b hb
    simp only [List.map_cons, List.map_nil, List.mem_cons, List.not_mem_nil, or_false] at hb
    subst hb
    simp only [List.mem_map] at ha
    obtain ⟨o, ho, rfl⟩ := ha
    intro he
    simp only [Prod.mk.injEq] at he
    exact hn o ho he.1 he.2

theorem delPair_nodup {l : List Pair} (x : Pair) (h : (l.map fun o => (o.a.val, o.b.val)).Nodup) :
    ((delPair l x).map fun o => (o.a.val, o.b.val)).Nodup :=
  h.sublist (List.filter_sublist.map _)

theorem applyWrite_unique {s : St} (w : Write) (h : OwnersUnique s) : OwnersUnique (applyWrite s w) := by
  cases w <;> simp only [OwnersUnique, applyWrite] at h ⊢
  · exact putPair_nodup _ h
  · exact delPair_nodup _ h
  all_goals exact h

theorem applyAll_unique {s : St} (ws : List Write) (h : OwnersUnique s) : OwnersUnique (applyAll s ws) := by
  induction ws generalizing s with
  | nil => exact h
  | cons w ws ih => exact ih (applyWrite_unique w h)

/-- Owner rows are unique in every reachable state. -/
theorem owners_unique {s : St} (h : Reachable s) : OwnersUnique s := by
  induction h with
  | init => simp [OwnersUnique]
  | step _ _ ih => exact applyAll_unique _ ih
  | admin _ _ _ _ _ ih => exact ih

/-! ## Owner counts -/

theorem putPair_count (l : List Pair) (x : Pair) (k : Nat) :
    (l.filter (·.a.val = k)).length ≤ ((putPair l x).filter (·.a.val = k)).length := by
  unfold putPair; split
  · exact le_refl _
  · simp [List.filter_append]

/-- Writes other than `DelOwner` never lower an owner count. -/
theorem count_mono {s : St} (w : Write) (hw : ∀ x, w ≠ .DelOwner x) (k : Nat) :
    ownerCount s k ≤ ownerCount (applyWrite s w) k := by
  cases w <;> simp only [ownerCount, applyWrite, le_refl]
  · exact putPair_count _ _ _
  · exact absurd rfl (hw _)

theorem count_mono_all {s : St} (ws : List Write) (hw : ∀ x, Write.DelOwner x ∉ ws) (k : Nat) :
    ownerCount s k ≤ ownerCount (applyAll s ws) k := by
  induction ws generalizing s with
  | nil => exact le_refl _
  | cons w ws ih =>
    have h1 : ∀ x, w ≠ .DelOwner x := fun x he => hw x (by simp [he])
    exact (count_mono w h1 k).trans (ih (fun x hx => hw x (by simp [hx])))

/-- A command deletes at most one owner row pattern, alone. -/
theorem dels_single (p : Principal) (s : Snapshot) (c : Command) ws r
    (h : transition p s c = .ok (.Ok (ws, r))) :
    (∀ x, Write.DelOwner x ∉ ws.val) ∨ ∃ x, ws.val = [.DelOwner x] := by
  refine of_spec (P := fun ws _ => (∀ x, Write.DelOwner x ∉ ws.val) ∨ ∃ x, ws.val = [.DelOwner x]) ?_ h
  walk
  all_goals (simp only [OnOk]; try trivial)
  all_goals simp_all

/-- While ownerless crates are not allowed, a crate that has an owner keeps
one through every successful command, in every reachable state. -/
theorem owner_remains (p : Principal) (s : Snapshot) (c : Command) ws r
    (hr : Reachable (Snapshot.toSt s)) (h : transition p s c = .ok (.Ok (ws, r)))
    (hal : (Snapshot.toSt s).allowOwnerless = false) (k : Nat)
    (hk : 1 ≤ ownerCount (Snapshot.toSt s) k) :
    1 ≤ ownerCount (applyAll (Snapshot.toSt s) ws.val) k := by
  rcases dels_single p s c ws r h with hn | ⟨x, hx⟩
  · exact hk.trans (count_mono_all _ hn k)
  · rw [hx]
    simp only [applyAll, List.foldl_cons, List.foldl_nil, applyWrite, ownerCount, delPair]
    by_cases hxk : x.a.val = k
    · subst hxk
      have h2 := owner_removal_needs_two p s c ws r h hal x (by simp [hx])
      exact owner_remains_of_unique _ _ x.b.val (owners_unique hr) h2
    · unfold ownerCount at hk
      rw [List.filter_filter]
      refine hk.trans (le_of_eq (congrArg List.length (List.filter_congr fun o _ => ?_)))
      by_cases ho : o.a.val = k
      · simp [ho, Ne.symm hxk]
      · simp [ho]

end kellnr_kernel.Invariants
