import Preview
/-!
# Scenarios

Concrete runs of `transition` where the guarded cases succeed (owner delete,
confirmed burn, unexpired read, right password), so the theorems are not
vacuous.
-/
open Aeneas Aeneas.Std Result wastebin_kernel wastebin_kernel.Spec wastebin_kernel.Commands
  wastebin_kernel.Theorems wastebin_kernel.Preview H5iAppLib

namespace wastebin_kernel.Scenarios

def who (uids : List U64) (now : U64) (h : uids.length ≤ Usize.max := by simp only [List.length_cons, List.length_nil]; scalar_tac) :
    Principal := ⟨alloc.vec.Vec.from uids h, now, 0#u64⟩

/-! ## Burn after reading, on `s0` -/

theorem bot_gets_confirmation :
    transition bot s0 (.View 7#u64 false none) ⦃ o => ∃ ws, o = .Ok (ws, .ConfirmBurn) ∧ ws.val = [] ⦄ := by
  simp only [transition]
  unfold view
  step*
  all_goals simp_all [s0, p0, vecOf]

theorem reader_burns :
    transition bot s0 (.View 7#u64 true none) ⦃ o => ∃ ws v, o = .Ok (ws, .Shown v) ∧
      v.burned = true ∧ ws.val = [.DelPaste 0#u64] ⦄ := by
  simp only [transition]
  unfold view read
  step*
  all_goals simp_all [s0, p0, bot, vecOf, isExpired]

/-- The state after the burn: the counter stays, the paste is gone. -/
def burnt : Snapshot := ⟨⟨1#u64, 1#u64⟩, vecOf []⟩

theorem burnt_after : applyAll (Snapshot.toSt s0) [.DelPaste 0#u64] = Snapshot.toSt burnt := by
  simp [applyAll, applyWrite, Snapshot.toSt, s0, p0, burnt, vecOf]

theorem second_read_fails :
    transition bot burnt (.View 7#u64 true none) ⦃ o => o = .Err .NotFound ⦄ := by
  simp only [transition]
  unfold view
  step*
  all_goals simp_all [burnt, vecOf]

/-- `/raw/{id}` has no confirmation step, so a preview of a raw link burns too. -/
theorem raw_link_burns :
    transition bot s0 (.Fetch 7#u64 none) ⦃ o => ∃ ws v, o = .Ok (ws, .Shown v) ∧
      v.burned = true ∧ ws.val = [.DelPaste 0#u64] ⦄ := by
  simp only [transition]
  unfold fetch read
  step*
  all_goals simp_all [s0, p0, bot, vecOf, isExpired]

theorem slug_taken :
    transition author s0 (.Create (vecOf []) none false none) ⦃ o => o = .Err .SlugTaken ⦄ := by
  simp only [transition]
  unfold create deadline
  step*
  all_goals simp_all [s0, p0, author, vecOf]

/-- A zero second expiry is rejected before any paste is written. -/
theorem zero_expiry_refused :
    transition author s0 (.Create (vecOf []) (some 0#u32) false none)
      ⦃ o => o = .Err .BadExpiry ⦄ := by
  h5i_eval (transition create deadline) [s0, p0, author, vecOf]

/-! ## Deleting -/

theorem owner_deletes :
    transition (who [3#u64, 1#u64] 100#u64) s0 (.Delete 7#u64) ⦃ o => ∃ ws, o = .Ok (ws, .Done) ∧
      ws.val = [.DelPaste 0#u64] ⦄ := by
  simp only [transition]
  unfold delete
  step*
  all_goals simp_all [s0, p0, who, vecOf]

theorem stranger_refused :
    transition (who [2#u64] 100#u64) s0 (.Delete 7#u64) ⦃ o => o = .Err .Forbidden ⦄ := by
  simp only [transition]
  unfold delete
  step*
  all_goals simp_all [s0, p0, who, vecOf]

theorem no_cookie_refused :
    transition (who [] 100#u64) s0 (.Delete 7#u64) ⦃ o => o = .Err .Forbidden ⦄ := by
  simp only [transition]
  unfold delete
  step*
  all_goals simp_all [s0, p0, who, vecOf]

/-! ## Expiry and passwords -/

/-- A paste that expires at time 50, and one locked with fingerprint 42. -/
def p1 : Paste := ⟨0#u64, 9#u64, 1#u64, vecOf [], some 50#u64, false, none⟩
def p2 : Paste := ⟨1#u64, 8#u64, 1#u64, vecOf [], none, false, some 42#u64⟩
def s1 : Snapshot := ⟨⟨2#u64, 1#u64⟩, vecOf [p1, p2]⟩

theorem served_until_expiry :
    transition (who [] 50#u64) s1 (.Fetch 9#u64 none) ⦃ o => ∃ ws v, o = .Ok (ws, .Shown v) ∧ ws.val = [] ⦄ := by
  simp only [transition]
  unfold fetch read
  step*
  all_goals simp_all [s1, p1, who, vecOf, isExpired]

theorem expired_is_gone :
    transition (who [] 51#u64) s1 (.Fetch 9#u64 none) ⦃ o => ∃ ws, o = .Ok (ws, .Gone) ∧
      ws.val = [.DelPaste 0#u64] ⦄ := by
  simp only [transition]
  unfold fetch read
  step*
  all_goals simp_all [s1, p1, who, vecOf, isExpired]

theorem purge_removes_expired :
    transition (who [] 51#u64) s1 .Purge ⦃ o => ∃ ws, o = .Ok (ws, .Done) ∧ ws.val = [.DelPaste 0#u64] ⦄ := by
  simp only [transition]
  apply WP.spec_mono (purge_spec _ _)
  rintro o ⟨ws, rfl, hws⟩
  refine ⟨ws, rfl, ?_⟩
  rw [hws]
  simp [s1, p1, p2, who, vecOf, isExpired]

theorem needs_password :
    transition (who [] 0#u64) s1 (.Fetch 8#u64 none) ⦃ o => o = .Err .NeedPassword ⦄ := by
  simp only [transition]
  unfold fetch read
  step*
  all_goals simp_all [s1, p1, p2, vecOf, isExpired]

theorem wrong_password :
    transition (who [] 0#u64) s1 (.Fetch 8#u64 (some 1#u64)) ⦃ o => o = .Err .WrongPassword ⦄ := by
  simp only [transition]
  unfold fetch read
  step*
  all_goals simp_all [s1, p1, p2, who, vecOf, isExpired]

theorem right_password :
    transition (who [] 0#u64) s1 (.Fetch 8#u64 (some 42#u64)) ⦃ o => ∃ ws v, o = .Ok (ws, .Shown v) ∧
      ws.val = [] ⦄ := by
  simp only [transition]
  unfold fetch read
  step*
  all_goals simp_all [s1, p1, p2, who, vecOf, isExpired]

end wastebin_kernel.Scenarios
