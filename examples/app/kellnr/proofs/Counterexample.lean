import Invariants
/-!
# PR #1243, machine-checked

A read-only non-admin owner of crate 7 changes its ACLs before PR #1243 and
is refused after, so theorem (a) fails for the old code. The last section
shows the fixed kernel still grants what its theorems restrict.
-/
open Aeneas Aeneas.Std Result kellnr_kernel kellnr_kernel.Spec kellnr_kernel.Theorems H5iAppLib

namespace kellnr_kernel.Counterexample

/-- User 1 is read-only and owns crate 7. -/
def s0 : Snapshot where
  settings := ⟨false, false⟩
  users := vecOf [⟨1#u64, false, true⟩]
  crates := vecOf [⟨7#u64, false⟩]
  versions := vecOf []
  owners := vecOf [⟨7#u64, 1#u64⟩]
  crate_users := vecOf []
  crate_groups := vecOf []
  group_members := vecOf []

def bySession : Principal := ⟨1#u64, .Session⟩
def byToken : Principal := ⟨1#u64, .Token⟩

theorem s0_facts : isReadOnly (Snapshot.toSt s0) 1 = true ∧ isAdmin (Snapshot.toSt s0) 1 = false := by
  decide

/-- Bug 1: a session login ignored the read-only flag. -/
theorem pre1243_session_adds_owner :
    transition_pre1243 bySession s0 (.AddOwner 7#u64 2#u64) ⦃ o =>
      ∃ ws r, o = .Ok (ws, r) ∧ ws.val = [.AddOwner ⟨7#u64, 2#u64⟩] ⦄ := by
  unfold transition_pre1243 run
  step*
  all_goals simp_all [gd, mu, userOf, own, canMod, isOwner, hasPair, s0, bySession, vecOf, Snapshot.toSt]

/-- Bug 2: the group endpoints skipped `check_can_modify`, even for tokens. -/
theorem pre1243_token_adds_group :
    transition_pre1243 byToken s0 (.AddCrateGroup 7#u64 3#u64) ⦃ o =>
      ∃ ws r, o = .Ok (ws, r) ∧ ws.val = [.AddCrateGroup ⟨7#u64, 3#u64⟩] ⦄ := by
  unfold transition_pre1243 run
  step*
  all_goals simp_all [gd, mu, userOf, own, canMod, isOwner, hasPair, s0, byToken, vecOf, Snapshot.toSt]

/-- The fixed kernel refuses both. -/
theorem fixed_refuses_session :
    transition bySession s0 (.AddOwner 7#u64 2#u64) ⦃ o => o = .Err .ReadOnlyModify ⦄ := by
  unfold transition run
  step*
  all_goals simp_all [gd, mu, userOf, canMod, s0, bySession, vecOf, Snapshot.toSt]

theorem fixed_refuses_group :
    transition byToken s0 (.AddCrateGroup 7#u64 3#u64) ⦃ o => o = .Err .ReadOnlyModify ⦄ := by
  unfold transition run
  step*
  all_goals simp_all [gd, mu, userOf, canMod, s0, byToken, vecOf, Snapshot.toSt]

/-- Theorem (a) is false for the code before PR #1243. -/
theorem pre1243_violates_read_only :
    ¬ ∀ (p : Principal) (s : Snapshot) (c : Command) ws r,
        transition_pre1243 p s c = .ok (.Ok (ws, r)) →
        isReadOnly (Snapshot.toSt s) p.user.val = true →
        isAdmin (Snapshot.toSt s) p.user.val = false → ws.val = [] := by
  intro hall
  obtain ⟨o, ho, ws, r, rfl, hws⟩ := (WP.spec_equiv_exists _ _).1 pre1243_session_adds_owner
  have := hall bySession s0 _ ws r ho s0_facts.1 s0_facts.2
  simp [hws] at this

/-! ## The fixed kernel still grants what it should -/

/-- User 2 can modify and owns crates 7 (with user 1) and 8. Crate 8 is
restricted; user 3 is its crate user. -/
def s1 : Snapshot where
  settings := ⟨false, false⟩
  users := vecOf [⟨1#u64, false, true⟩, ⟨2#u64, false, false⟩, ⟨3#u64, false, false⟩]
  crates := vecOf [⟨7#u64, false⟩, ⟨8#u64, true⟩]
  versions := vecOf []
  owners := vecOf [⟨7#u64, 1#u64⟩, ⟨7#u64, 2#u64⟩, ⟨8#u64, 2#u64⟩]
  crate_users := vecOf [⟨8#u64, 3#u64⟩]
  crate_groups := vecOf []
  group_members := vecOf []

def writer : Principal := ⟨2#u64, .Session⟩

/-- A writable owner adds an owner, even through a session. -/
theorem fixed_writer_adds_owner :
    transition writer s1 (.AddOwner 7#u64 4#u64) ⦃ o =>
      ∃ ws r, o = .Ok (ws, r) ∧ ws.val = [.AddOwner ⟨7#u64, 4#u64⟩] ⦄ := by
  unfold transition run
  step*
  all_goals simp_all [gd, mu, userOf, own, canMod, isOwner, hasPair, s1, writer, vecOf, Snapshot.toSt]

/-- Crate 7 has two owners, so one can be removed. -/
theorem fixed_writer_removes_owner :
    transition writer s1 (.RemoveOwner 7#u64 1#u64) ⦃ o =>
      ∃ ws r, o = .Ok (ws, r) ∧ ws.val = [.DelOwner ⟨7#u64, 1#u64⟩] ⦄ := by
  unfold transition run
  step*
  all_goals simp_all [gd, mu, userOf, own, canMod, isOwner, hasPair, s1, writer, vecOf, Snapshot.toSt]

/-- Crate 8 has one owner, so it stays. -/
theorem fixed_keeps_last_owner :
    transition writer s1 (.RemoveOwner 8#u64 2#u64) ⦃ o => o = .Err .LastOwner ⦄ := by
  unfold transition run
  step*
  all_goals simp_all [gd, mu, userOf, own, canMod, isOwner, hasPair, s1, writer, vecOf, Snapshot.toSt]

/-- A crate user's token downloads a restricted crate. -/
theorem fixed_crate_user_downloads :
    transition ⟨3#u64, .Token⟩ s1 (.Download 8#u64) ⦃ o => ∃ ws, o = .Ok (ws, .File) ⦄ := by
  unfold transition run
  simp only [check_download_auth_eq]
  step*
  all_goals simp_all [dl, tu, mu, userOf, restricted, isCrateUser, hasPair, s1, vecOf, Snapshot.toSt]

/-- Publishing a new crate makes the publisher its first owner. -/
theorem fixed_publish_makes_owner :
    transition ⟨3#u64, .Token⟩ s1 (.Publish 9#u64 1#u64) ⦃ o =>
      ∃ ws r, o = .Ok (ws, r) ∧
        ws.val = [.AddCrate ⟨9#u64, false⟩, .AddVersion ⟨9#u64, 1#u64, false⟩, .AddOwner ⟨9#u64, 3#u64⟩] ⦄ := by
  unfold transition run
  step*
  all_goals simp_all [tu, mu, userOf, canMod, s1, vecOf, Snapshot.toSt]

/-- A fresh registry where user 3 can publish. -/
def s2 : Snapshot where
  settings := ⟨false, false⟩
  users := vecOf [⟨3#u64, false, false⟩]
  crates := vecOf []
  versions := vecOf []
  owners := vecOf []
  crate_users := vecOf []
  crate_groups := vecOf []
  group_members := vecOf []

/-- Publishing into the fresh registry reaches a state where crate 9 has an
owner, so `Invariants.owner_remains` applies to a real state. -/
theorem published_reachable : ∃ t, Reachable t ∧ ownerCount t 9 = 1 := by
  have hp : transition ⟨3#u64, .Token⟩ s2 (.Publish 9#u64 1#u64) ⦃ o =>
      ∃ ws r, o = .Ok (ws, r) ∧
        ws.val = [.AddCrate ⟨9#u64, false⟩, .AddVersion ⟨9#u64, 1#u64, false⟩, .AddOwner ⟨9#u64, 3#u64⟩] ⦄ := by
    unfold transition run
    step*
    all_goals simp_all [tu, mu, userOf, canMod, s2, vecOf, Snapshot.toSt]
  obtain ⟨o, ho, ws, r, rfl, hws⟩ := (WP.spec_equiv_exists _ _).1 hp
  have h0 : Reachable (Snapshot.toSt s2) := Reachable.init [⟨3#u64, false, false⟩] [] false false
  refine ⟨_, Reachable.step h0 ho, ?_⟩
  rw [hws]
  decide

end kellnr_kernel.Counterexample
