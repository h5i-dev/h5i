import Lemmas
import H5iAppLib
/-! Specs for the non-loop helpers of `transition`. -/
open Aeneas Aeneas.Std Result docs_kernel docs_kernel.Spec H5iAppLib

namespace docs_kernel.TransitionLemmas

@[step]
theorem status_eq_spec (x y : Status) :
    Status.Insts.CoreCmpPartialEqStatus.eq x y ⦃ b => b = true ↔ x = y ⦄ := by
  cases x <;> cases y <;> simp [Status.Insts.CoreCmpPartialEqStatus.eq, Status.read_discriminant]

@[step]
theorem role_ne_spec (x y : Role) :
    core.cmp.PartialEq.ne.trait_default Role.Insts.CoreCmpPartialEqRole x y ⦃ b => b = true ↔ x ≠ y ⦄ := by
  unfold core.cmp.PartialEq.ne.trait_default core.cmp.PartialEq.ne.default
  cases x <;> cases y <;> simp [Role.Insts.CoreCmpPartialEqRole.eq, Role.read_discriminant]

@[step]
theorem status_ne_spec (x y : Status) :
    core.cmp.PartialEq.ne.trait_default Status.Insts.CoreCmpPartialEqStatus x y ⦃ b => b = true ↔ x ≠ y ⦄ := by
  unfold core.cmp.PartialEq.ne.trait_default core.cmp.PartialEq.ne.default
  cases x <;> cases y <;> simp [Status.Insts.CoreCmpPartialEqStatus.eq, Status.read_discriminant]

@[step]
theorem one_spec (w : Write) : one w ⦃ v => v.val = [w] ⦄ := by
  unfold one
  step*

/-- `authorized_doc` as a list function: hidden and missing documents both
give `NotFound`. -/
def authDoc (s : St) (u id : Nat) (a : Action) : core.result.Result Document Error :=
  match findDoc s.docs id with
  | none => .Err .NotFound
  | some d =>
    if allowed s u d.project.val .Read then
      if allowed s u d.project.val a then .Ok d else .Err .Forbidden
    else .Err .NotFound

theorem findDoc_some {ds : List Document} {id : Nat} {d : Document} (h : findDoc ds id = some d) :
    d ∈ ds ∧ d.id.val = id := by
  unfold findDoc at h
  exact ⟨List.mem_of_find?_eq_some h, by simpa using List.find?_some h⟩

theorem authDoc_ok {s : St} {u id : Nat} {a : Action} {d : Document} (h : authDoc s u id a = .Ok d) :
    findDoc s.docs id = some d ∧ allowed s u d.project.val .Read ∧ allowed s u d.project.val a := by
  unfold authDoc at h
  split at h
  · cases h
  · rename_i d' hd
    split at h <;> [split at h; cases h] <;> cases h
    exact ⟨hd, by assumption, by assumption⟩

@[step]
theorem authorized_doc_spec (s : Snapshot) (u id : U64) (a : Action) :
    authorized_doc s u id a ⦃ r => r = authDoc (Snapshot.toSt s) u.val id.val a ⦄ := by
  unfold authorized_doc
  step as ⟨o, ho⟩
  simp only [authDoc, Snapshot.toSt, ← ho]
  cases o <;> step* <;> simp_all [Snapshot.toSt]

@[step]
theorem fresh_id_spec (s : Snapshot) :
    fresh_id s ⦃ r => match r with
      | .Err e => e = .Overflow ∧ s.counter.next_id.val = U64.max
      | .Ok (id, c) => id = s.counter.next_id ∧ c.next_id.val = s.counter.next_id.val + 1 ⦄ := by
  unfold fresh_id
  step*
  all_goals (simp_all [U64.rMax, U64.max_eq]; try scalar_tac)

@[step]
theorem with_status_spec (d : Document) (st : Status) (ap : Option U64) :
    with_status d st ap ⦃ r =>
      (d.version.val = U64.max ∧ r = .Err .Overflow) ∨
      (∃ v : U64, v.val = d.version.val + 1 ∧
        r = .Ok { d with status := st, approver := ap, version := v }) ⦄ := by
  unfold with_status
  step*
  all_goals (simp_all [U64.rMax, U64.max_eq]; try scalar_tac)

/-- Only owners may approve or manage, and owners may also write. -/
theorem allowed_write_of {s : St} {u p : Nat} {a : Action}
    (ha : a = .Approve ∨ a = .Manage) (h : allowed s u p a) : allowed s u p .Write := by
  unfold allowed at *
  split at h <;> rename_i r _ <;> [cases r <;> rcases ha with rfl | rfl <;> simp_all [policy]; simp at h]

end docs_kernel.TransitionLemmas
