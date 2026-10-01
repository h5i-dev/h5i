import Lemmas
/-!
# Searches show what the template says, for every name

`search_exact`: for any well-formed template and any caller name, a search
returns exactly the records matching the template with the name in its holes.
With the template `owner='$'`, that is exactly the caller's records
(`search_own`, `search_all_own`), whatever bytes the name contains.
-/
open Aeneas Aeneas.Std Result H5iAppLib filters_kernel

namespace Filters

theorem search_spec (a : Principal) (snap : Snapshot) (rule : U64) (pre : Bool) :
    search a snap rule pre ⦃ res => match snap.rules.val.find? (fun r => r.id = rule) with
      | none => res = .Err .NoRule
      | some r => RunPost pre a.name.val r.template.val snap.records.val res ⦄ := by
  unfold search
  step with find_rule_spec as ⟨o, ho⟩
  rw [vec_deref_val] at ho
  rw [ho]
  cases o <;> (step*; try simp_all [vec_deref_val])

theorem transition_search (a : Principal) (snap : Snapshot) (rule : U64) :
    transition a snap (.Search rule) = search a snap rule false := rfl

theorem transition_pre_search (a : Principal) (snap : Snapshot) (rule : U64) :
    transition_pre a snap (.Search rule) = search a snap rule true := rfl

/-- A successful search returns exactly the records that match the template
with the caller's name in its holes. -/
theorem search_exact (a : Principal) (snap : Snapshot) (rule : U64) (r : Rule) (ts : List TClause)
    (hr : snap.rules.val.find? (fun x => x.id = rule) = some r)
    (ht : r.template.val = renderT ts) (hw : WellFormed ts)
    {ws : alloc.vec.Vec Write} {rs : alloc.vec.Vec Record}
    (h : transition a snap (.Search rule) = ok (.Ok (ws, .Records rs))) :
    rs.val = snap.records.val.filter (matchesM (ts.map (inst a.name.val))) := by
  have hp := post_of_ok (search_spec a snap rule false) (by rw [← transition_search]; exact h)
  rw [hr] at hp
  simp only [RunPost, Bool.false_eq_true, if_false, ht, parse_substitute _ _ hw] at hp
  split_ifs at hp
  obtain ⟨rs', he, hrs⟩ := hp
  simp only [core.result.Result.Ok.injEq, Prod.mk.injEq, Reply.Records.injEq] at he
  rw [he.2, hrs]

/-- A search through a well-formed template that fits never fails. -/
theorem search_succeeds (a : Principal) (snap : Snapshot) (rule : U64) (r : Rule) (ts : List TClause)
    (hr : snap.rules.val.find? (fun x => x.id = rule) = some r)
    (ht : r.template.val = renderT ts) (hw : WellFormed ts)
    (hlt : r.template.val.length ≤ 4096) (hln : a.name.val.length ≤ 4096) :
    ∃ rs, transition a snap (.Search rule) = ok (.Ok (alloc.vec.Vec.new Write, .Records rs)) := by
  obtain ⟨res, hres, hp⟩ := (WP.spec_equiv_exists _ _).1 (search_spec a snap rule false)
  rw [hr] at hp
  simp only [RunPost, Bool.false_eq_true, if_false, ht, parse_substitute _ _ hw] at hp
  rw [if_neg (by rw [← ht]; omega)] at hp
  obtain ⟨rs, rfl, -⟩ := hp
  exact ⟨rs, by rw [transition_search, hres]⟩

/-! ## The owner template -/

/-- `owner='$'`. -/
def ownerTpl : List TClause := [(ownerKey, [.hole])]

theorem ownerTpl_wf : WellFormed ownerTpl := by
  obtain ⟨hq, hb, hbar, he, hh⟩ := bytes_val
  intro t ht
  simp only [ownerTpl, List.mem_singleton] at ht
  subst ht
  refine ⟨?_, ?_, by simp⟩ <;> intro b hb' <;> simp only [ownerKey, List.mem_cons, List.not_mem_nil, or_false] at hb' <;>
    rcases hb' with rfl | rfl | rfl | rfl | rfl <;> simp only [hq, hb, hbar, he, hh] <;> decide

theorem matches_owner (nm : List U8) (x : Record) :
    matchesM (ownerTpl.map (inst nm)) x = decide (x.owner.val = nm) := by
  simp [matchesM, ownerTpl, inst, fieldM]

/-- Through `owner='$'`, a search shows only the caller's records. -/
theorem search_own (a : Principal) (snap : Snapshot) (rule : U64) (r : Rule)
    (hr : snap.rules.val.find? (fun x => x.id = rule) = some r) (ht : r.template.val = renderT ownerTpl)
    {ws : alloc.vec.Vec Write} {rs : alloc.vec.Vec Record}
    (h : transition a snap (.Search rule) = ok (.Ok (ws, .Records rs))) :
    ∀ x ∈ rs.val, x.owner.val = a.name.val := by
  rw [search_exact a snap rule r ownerTpl hr ht ownerTpl_wf h]
  intro x hx
  simpa [matches_owner] using (List.mem_filter.1 hx).2

/-- ... and all of them. -/
theorem search_all_own (a : Principal) (snap : Snapshot) (rule : U64) (r : Rule)
    (hr : snap.rules.val.find? (fun x => x.id = rule) = some r) (ht : r.template.val = renderT ownerTpl)
    {ws : alloc.vec.Vec Write} {rs : alloc.vec.Vec Record}
    (h : transition a snap (.Search rule) = ok (.Ok (ws, .Records rs))) :
    ∀ x ∈ snap.records.val, x.owner.val = a.name.val → x ∈ rs.val := by
  rw [search_exact a snap rule r ownerTpl hr ht ownerTpl_wf h]
  intro x hx ho
  exact List.mem_filter.2 ⟨hx, by simp [matches_owner, ho]⟩

/-! ## Writes -/

/-- Records are written with the caller as owner; rules only by admins. -/
def Allowed (a : Principal) : Write → Prop
  | .Record x => x.owner = a.name
  | .Rule _ => a.is_admin = true

theorem transition_writes (a : Principal) (snap : Snapshot) (c : Command) :
    transition a snap c ⦃ OnOk (fun ws _ => ∀ w ∈ ws.val, Allowed a w) ⦄ := by
  cases c with
  | Search rule =>
    rw [transition_search]
    apply WP.spec_mono (search_spec a snap rule false)
    intro res hp
    split at hp
    · subst hp; trivial
    · simp only [RunPost, Bool.false_eq_true, if_false] at hp
      split_ifs at hp
      · subst hp; trivial
      · split at hp
        · subst hp; trivial
        · obtain ⟨rs, rfl, -⟩ := hp; simp [OnOk]
  | Add tag =>
    unfold transition step
    step*
    all_goals simp_all [OnOk, Allowed]
  | SetRule id tpl =>
    unfold transition step
    step*
    all_goals simp_all [OnOk, Allowed]

/-- Every write of every successful command is allowed. -/
theorem writes_authorized : WritesAuthorized transition id (fun ws => ws.val) (fun _ a w => Allowed a w) :=
  writesAuthorized_of_spec transition_writes

end Filters
