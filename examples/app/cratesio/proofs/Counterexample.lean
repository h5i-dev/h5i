import Theorems
/-!
# PR #14760, machine-checked

User 1 is locked. Before the PR, the OAuth callback still wrote a session
for them; after it, the callback refuses. `locked_commits_nothing` is
therefore false for the old code.
-/
open Aeneas Aeneas.Std Result cratesio_kernel cratesio_kernel.Spec cratesio_kernel.Theorems H5iAppLib

namespace cratesio_kernel.Counterexample

/-- User 1 is locked with no end date. -/
def s0 : Snapshot where
  counter := ⟨0#u64, 0#u64⟩
  users := vecOf [⟨1#u64, false, true, 0#u64, true⟩]
  sessions := vecOf []
  tokens := vecOf []
  crates := vecOf []
  versions := vecOf []
  owners := vecOf []
  invites := vecOf []
  deps := vecOf []

/-- GitHub vouches for user 1 at time 1000. -/
def gh : Principal := ⟨1#u64, 1#u64, .GitHub, 1000#u64, vecOf []⟩

theorem s0_locked : lockedNow (Snapshot.toSt s0) gh := ⟨_, rfl, rfl⟩

/-- Before the PR: the locked user gets a session. -/
theorem pre14760_signs_in :
    transition_pre14760 gh s0 .Authorize ⦃ o =>
      ∃ ws r, o = .Ok (ws, r) ∧ ws.val = [.PutSession ⟨0#u64, 1#u64⟩, .SetCounter ⟨1#u64, 0#u64⟩] ⦄ := by
  unfold transition_pre14760 run authorize
  step*
  all_goals simp_all [s0, gh, vecOf, core.num.U64.MAX, U64.rMax]
  all_goals scalar_tac

/-- After the PR: refused with the lock. -/
theorem fixed_refuses :
    transition gh s0 .Authorize ⦃ o => o = .Err .AccountLocked ⦄ := by
  unfold transition run authorize
  step*
  all_goals simp_all [s0, gh, vecOf, lockedAt, core.num.U64.MAX, U64.rMax]
  all_goals scalar_tac

/-- `locked_commits_nothing` does not hold before the PR. -/
theorem pre14760_violates_lock :
    ¬ ∀ (p : Principal) (s : Snapshot) (c : Command) ws r,
        transition_pre14760 p s c = .ok (.Ok (ws, r)) → p.via ≠ .Operator →
        lockedNow (Snapshot.toSt s) p → ws.val = [] := by
  intro hall
  obtain ⟨o, ho, ws, r, rfl, hws⟩ := (WP.spec_equiv_exists _ _).1 pre14760_signs_in
  have := hall gh s0 _ ws r ho (by simp [gh]) s0_locked
  simp [hws] at this

end cratesio_kernel.Counterexample
