import Theorems
/-!
# Before the fix, a name rewrites the filter

`substitute_pre` escapes `\` but not `'`. Eve signs up with the name
`x'|owner='bob`; through `owner='$'` her search becomes
`owner='x'|owner='bob'`, and she sees Bob's records (`pre_leaks`). The fixed
kernel shows her none (`fixed_hides`), as `search_own` says it must.
-/
open Aeneas Aeneas.Std Result H5iAppLib filters_kernel

namespace Filters

/-- `bob`. -/
def bob : List U8 := [98#u8, 111#u8, 98#u8]

/-- `x'|owner='bob`. -/
def eveName : List U8 :=
  [120#u8, 39#u8, 124#u8, 111#u8, 119#u8, 110#u8, 101#u8, 114#u8, 61#u8, 39#u8, 98#u8, 111#u8, 98#u8]

/-- `owner='$'`. -/
def ownerText : List U8 := [111#u8, 119#u8, 110#u8, 101#u8, 114#u8, 61#u8, 39#u8, 36#u8, 39#u8]

theorem ownerText_eq : renderT ownerTpl = ownerText := by
  obtain ⟨hq, -, -, he, hh⟩ := bytes_val
  simp [renderT, ownerTpl, tclauseText, pieceText, ownerKey, ownerText, hq, he, hh]

def bobRec : Record := { id := 1#u64, owner := vecOf bob (by simp [bob]; scalar_tac), tag := vecOf [116#u8] }

def ownerRule : Rule := { id := 1#u64, template := vecOf ownerText (by simp [ownerText]; scalar_tac) }

def snap0 : Snapshot := { rules := vecOf [ownerRule], records := vecOf [bobRec], next_id := 2#u64 }

def eve : Principal := ⟨vecOf eveName (by simp only [eveName, List.length_cons, List.length_nil]; scalar_tac), false⟩

theorem snap0_find : snap0.rules.val.find? (fun r => r.id = 1#u64) = some ownerRule := by
  simp [snap0, ownerRule]

theorem ownerRule_text : ownerRule.template.val = renderT ownerTpl := by
  rw [ownerText_eq]; simp [ownerRule]

/-- The text `substitute_pre` builds, read by the parser: two clauses, the
second naming Bob. -/
theorem pre_parse : parseM (substM escPreM eveName ownerText) = some [(ownerKey, [120#u8]), (ownerKey, bob)] := by
  obtain ⟨hq, hb, hbar, he, hh⟩ := bytes_val
  simp [parseM, substM, escPreM, escPreB, iterRun, pstep, pfin, eveName, ownerText, ownerKey, bob,
    hq, hb, hbar, he, hh]

theorem pre_leaks : ∃ ws rs, transition_pre eve snap0 (.Search 1#u64) = ok (.Ok (ws, .Records rs)) ∧
    bobRec ∈ rs.val := by
  obtain ⟨res, hres, hp⟩ := (WP.spec_equiv_exists _ _).1 (search_spec eve snap0 1#u64 true)
  rw [snap0_find] at hp
  simp only [RunPost, if_true, ownerRule, eve, vecOf_val, pre_parse] at hp
  rw [if_neg (by simp [ownerText, eveName])] at hp
  obtain ⟨rs, rfl, hrs⟩ := hp
  refine ⟨_, rs, by rw [transition_pre_search, hres], ?_⟩
  rw [hrs]
  simp [snap0, matchesM, fieldM, bobRec]

theorem fixed_hides {ws : alloc.vec.Vec Write} {rs : alloc.vec.Vec Record}
    (h : transition eve snap0 (.Search 1#u64) = ok (.Ok (ws, .Records rs))) : rs.val = [] := by
  rw [search_exact eve snap0 1#u64 ownerRule ownerTpl snap0_find ownerRule_text ownerTpl_wf h,
    show eve.name.val = eveName by simp [eve]]
  simp [snap0, matches_owner, bobRec, eveName, bob]

end Filters
