import Counterexample
/-!
# Scenarios

Concrete runs of the extracted kernel: guarded actions succeed for the right
caller and fail for the wrong one, so the theorems are not vacuous.
-/
open Aeneas Aeneas.Std Result cratesio_kernel cratesio_kernel.Spec cratesio_kernel.Counterexample H5iAppLib

namespace cratesio_kernel.Scenarios

/-- Crate 10 was published at time 0 by Alice (user 1) and is also owned by
team 7. Bob (user 2) is in team 7. Carol (user 3) is locked. Alice has token 1
(yank, crate 11 only) and token 2 (yank, crate 10 only). -/
def reg : Snapshot where
  counter := ⟨4#u64, 3#u64⟩
  users := vecOf [⟨1#u64, false, false, 0#u64, true⟩, ⟨2#u64, false, false, 0#u64, true⟩,
    ⟨3#u64, false, true, 0#u64, true⟩]
  sessions := vecOf [⟨1#u64, 1#u64⟩, ⟨2#u64, 2#u64⟩, ⟨3#u64, 3#u64⟩]
  tokens := vecOf [⟨1#u64, 1#u64, false, false, false, true, false, some 11#u64, 0#u64, false⟩,
    ⟨2#u64, 1#u64, false, false, false, true, false, some 10#u64, 0#u64, false⟩]
  crates := vecOf [⟨10#u64, 0#u64⟩]
  versions := vecOf [⟨10#u64, 1#u64, false, 1#u64⟩]
  owners := vecOf [⟨10#u64, 1#u64, false⟩, ⟨10#u64, 7#u64, true⟩]
  invites := vecOf []
  deps := vecOf []

def alice : Principal := ⟨1#u64, 1#u64, .Cookie 1#u64, 100#u64, vecOf []⟩
def bob : Principal := ⟨1#u64, 2#u64, .Cookie 2#u64, 100#u64, vecOf [7#u64]⟩
def carol : Principal := ⟨1#u64, 3#u64, .Cookie 3#u64, 100#u64, vecOf []⟩

/-! The loop helpers as equations, so `simp` can run them on concrete lists. -/

section
open Lemmas

theorem ev_find_user (v : alloc.vec.Vec User) (k : U64) : find_user v k = ok (v.val.find? (·.id = k)) :=
  eq_ok_of_spec (find_user_spec v k)
theorem ev_find_session (v : alloc.vec.Vec Session) (k : U64) : find_session v k = ok (v.val.find? (·.id = k)) :=
  eq_ok_of_spec (find_session_spec v k)
theorem ev_find_token (v : alloc.vec.Vec Token) (k : U64) : find_token v k = ok (v.val.find? (·.id = k)) :=
  eq_ok_of_spec (find_token_spec v k)
theorem ev_find_crate (v : alloc.vec.Vec Krate) (k : U64) : find_crate v k = ok (v.val.find? (·.id = k)) :=
  eq_ok_of_spec (find_crate_spec v k)
theorem ev_find_version (v : alloc.vec.Vec Version) (k n : U64) :
    find_version v k n = ok (v.val.find? (fun x => x.krate = k ∧ x.num = n)) :=
  eq_ok_of_spec (find_version_spec v k n)
theorem ev_find_invite (v : alloc.vec.Vec Invite) (k u : U64) :
    find_invite v k u = ok (v.val.find? (fun x => x.krate = k ∧ x.user = u)) :=
  eq_ok_of_spec (find_invite_spec v k u)
theorem ev_has_owner (v : alloc.vec.Vec Owner) (k o : U64) (t : Bool) :
    has_owner v k o t = ok (v.val.any (fun x => decide (x.krate = k ∧ x.owner = o ∧ x.team = t))) :=
  eq_ok_of_spec (has_owner_spec v k o t)
theorem ev_is_member (v : alloc.vec.Vec U64) (t : U64) : is_member v t = ok (decide (t ∈ v.val)) :=
  eq_ok_of_spec (is_member_spec v t)
theorem ev_other_user_owner (v : alloc.vec.Vec Owner) (k o : U64) (t : Bool) :
    other_user_owner v k o t = ok (otherUserOwner v.val k o t) :=
  eq_ok_of_spec (other_user_owner_spec v k o t)
theorem ev_has_reverse_dep (v : alloc.vec.Vec Dep) (k : U64) :
    has_reverse_dep v k = ok (v.val.any (fun d => decide (d.on = k ∧ d.krate ≠ k))) :=
  eq_ok_of_spec (has_reverse_dep_spec v k)
theorem ev_crate_exists (v : alloc.vec.Vec Krate) (k : U64) : crate_exists v k = ok (v.val.any (·.id = k)) :=
  eq_ok_of_spec (crate_exists_spec v k)
theorem ev_deps_known (cs : alloc.vec.Vec Krate) (ds : alloc.vec.Vec U64) :
    deps_known cs ds = ok (ds.val.all (fun d => cs.val.any (·.id = d))) :=
  eq_ok_of_spec (deps_known_spec cs ds)
theorem ev_is_locked (u : User) (now : U64) : is_locked u now = ok (lockedAt u now) :=
  eq_ok_of_spec (is_locked_spec u now)
theorem ev_token_live (t : Token) (now : U64) : token_live t now = ok (live t now) :=
  eq_ok_of_spec (token_live_spec t now)
theorem ev_endpoint_ok (t : Token) (e : Endpoint) : endpoint_ok t e = ok (endpointOk t e) :=
  eq_ok_of_spec (endpoint_ok_spec t e)
theorem ev_crate_ok (t : Token) (k : Option U64) : crate_ok t k = ok (crateOk t k) :=
  eq_ok_of_spec (crate_ok_spec t k)
theorem ev_team_owner_in (v : alloc.vec.Vec Owner) (k : U64) (teams : alloc.vec.Vec U64) :
    team_owner_in v k teams = ok (v.val.any (fun o => o.krate = k && o.team && decide (o.owner ∈ teams.val))) :=
  eq_ok_of_spec (team_owner_in_spec v k teams)

end

/-- Unfold the guard helpers and run every lookup. -/
macro "run_kernel" : tactic => `(tactic| (
  simp only [transition, run, authenticate, signed_in, check_scope, need_full, owner_guard, publish, publish_new,
    publish_update, publish_checks, yank, invite_owner, add_team, remove_owner, handle_invite, delete_crate,
    rights, authorize, operator]
  simp [ev_find_user, ev_find_session, ev_find_token, ev_find_crate, ev_find_version, ev_find_invite,
    ev_has_owner, ev_is_member, ev_other_user_owner, ev_has_reverse_dep, ev_crate_exists, ev_deps_known,
    ev_is_locked, ev_token_live, ev_endpoint_ok, ev_crate_ok, ev_team_owner_in, Snapshot.toSt,
    lockedAt, live, endpointOk, crateOk, Lemmas.otherUserOwner, reg, vecOf, alice, bob, carol]))

/-- Finish a scenario whose command succeeds. -/
macro "ok_writes" : tactic => `(tactic| (
  step*
  all_goals first
    | (simp_all [Lemmas.depWrites, Spec.age]; done)
    | (refine ⟨_, ⟨_, rfl⟩, ?_⟩; simp_all [Lemmas.depWrites])
    | scalar_tac))

/-- Bob publishes a new version through team 7 (Publish rights). -/
theorem team_member_publishes :
    transition bob reg (.Publish 10#u64 2#u64 (vecOf [])) ⦃ o =>
      ∃ ws r, o = .Ok (ws, r) ∧ ws.val = [.PutVersion ⟨10#u64, 2#u64, false, 2#u64⟩] ⦄ := by
  run_kernel
  all_goals ok_writes

/-- Bob cannot invite owners: team members have Publish rights, not Full. -/
theorem team_member_cannot_invite :
    transition bob reg (.InviteOwner 10#u64 2#u64) ⦃ o => o = .Err .TeamMember ⦄ := by
  run_kernel

/-- Alice, a user owner, invites Bob; the invitation expires in 30 days. -/
theorem owner_invites :
    transition alice reg (.InviteOwner 10#u64 2#u64) ⦃ o =>
      ∃ ws r, o = .Ok (ws, r) ∧ ws.val = [.PutInvite ⟨10#u64, 2#u64, 1#u64, 2592100#u64⟩] ⦄ := by
  run_kernel
  step*
  refine ⟨_, ⟨_, rfl⟩, ?_⟩
  have : expires = 2592100#u64 := by
    have := expires_post (by scalar_tac)
    scalar_tac
  simp_all

/-- Alice cannot remove herself: she is the only user owner (team 7 does not count). -/
theorem last_user_owner_stays :
    transition alice reg (.RemoveOwner 10#u64 1#u64 false) ⦃ o => o = .Err .LastUserOwner ⦄ := by
  run_kernel

/-- Alice's token 1 is scoped to crate 11, so it cannot yank crate 10. -/
theorem token_out_of_scope :
    transition ⟨1#u64, 1#u64, .Token 1#u64, 100#u64, vecOf []⟩ reg (.Yank 10#u64 1#u64 true) ⦃ o =>
      o = .Err .ScopeMismatch ⦄ := by
  run_kernel

/-- Her token 2 is scoped to crate 10 and yanks it. -/
theorem token_in_scope :
    transition ⟨1#u64, 1#u64, .Token 2#u64, 100#u64, vecOf []⟩ reg (.Yank 10#u64 1#u64 true) ⦃ o =>
      ∃ ws r, o = .Ok (ws, r) ∧ ws.val = [.PutVersion ⟨10#u64, 1#u64, true, 1#u64⟩] ⦄ := by
  run_kernel
  all_goals ok_writes

/-- Carol's account is locked: even a valid session does nothing. -/
theorem locked_session_refused :
    transition carol reg (.Publish 20#u64 1#u64 (vecOf [])) ⦃ o => o = .Err .AccountLocked ⦄ := by
  run_kernel

/-- Alice deletes crate 10 within 72 hours of publishing it. -/
theorem owner_deletes_new_crate :
    transition alice reg (.DeleteCrate 10#u64 0#u64) ⦃ o =>
      ∃ ws r, o = .Ok (ws, r) ∧ ws.val = [.DelCrate 10#u64] ⦄ := by
  run_kernel
  all_goals ok_writes

/-- Four days later, the crate has two owners (Alice and team 7), so it stays. -/
theorem old_shared_crate_stays :
    transition ⟨1#u64, 1#u64, .Cookie 1#u64, 345600#u64, vecOf []⟩ reg (.DeleteCrate 10#u64 0#u64) ⦃ o =>
      o = .Err .MultipleOwners ⦄ := by
  run_kernel
  step*
  all_goals simp_all [Spec.age]

/-! ## A locked account is reachable

User 1 signs up, then the operator locks the account. So the hypothesis of
`locked_commits_nothing` holds in reachable states, not only in `s0`. -/

def empty : Snapshot := ⟨⟨0#u64, 0#u64⟩, vecOf [], vecOf [], vecOf [], vecOf [], vecOf [], vecOf [], vecOf [], vecOf []⟩

def signedUp : Snapshot :=
  ⟨⟨1#u64, 0#u64⟩, vecOf [⟨1#u64, false, false, 0#u64, false⟩], vecOf [⟨0#u64, 1#u64⟩], vecOf [], vecOf [], vecOf [], vecOf [],
    vecOf [], vecOf []⟩

def lockedReg : Snapshot :=
  ⟨⟨1#u64, 0#u64⟩, vecOf [⟨1#u64, false, true, 0#u64, false⟩], vecOf [⟨0#u64, 1#u64⟩], vecOf [], vecOf [], vecOf [], vecOf [],
    vecOf [], vecOf []⟩

def op : Principal := ⟨1#u64, 0#u64, .Operator, 1000#u64, vecOf []⟩

theorem sign_up_step :
    transition gh empty .Authorize ⦃ o => ∃ ws r, o = .Ok (ws, r) ∧
      ws.val = [.PutUser ⟨1#u64, false, false, 0#u64, false⟩, .PutSession ⟨0#u64, 1#u64⟩, .SetCounter ⟨1#u64, 0#u64⟩] ⦄ := by
  simp only [gh, empty]
  run_kernel
  step*
  all_goals first
    | (simp only [core.num.U64.MAX, U64.rMax] at *; scalar_tac)
    | (refine ⟨_, ⟨_, rfl⟩, ?_⟩; simp_all; scalar_tac)

theorem lock_step :
    transition op signedUp (.Lock 1#u64 0#u64) ⦃ o => ∃ ws r, o = .Ok (ws, r) ∧
      ws.val = [.PutUser ⟨1#u64, false, true, 0#u64, false⟩] ⦄ := by
  simp only [op, signedUp]
  run_kernel
  step*

theorem locked_reachable : Reachable (Snapshot.toSt lockedReg) ∧ lockedNow (Snapshot.toSt lockedReg) gh := by
  refine ⟨?_, ⟨_, rfl, rfl⟩⟩
  obtain ⟨o1, h1, ws1, r1, rfl, hw1⟩ := (WP.spec_equiv_exists _ _).1 sign_up_step
  obtain ⟨o2, h2, ws2, r2, rfl, hw2⟩ := (WP.spec_equiv_exists _ _).1 lock_step
  have e0 : Snapshot.toSt empty = init := rfl
  have r0 : Reachable (Snapshot.toSt empty) := e0 ▸ .init
  have e1 : applyAll (Snapshot.toSt empty) ws1.val = Snapshot.toSt signedUp := by
    rw [hw1]; rfl
  have r1' : Reachable (Snapshot.toSt signedUp) := e1 ▸ Reachable.step r0 h1
  have e2 : applyAll (Snapshot.toSt signedUp) ws2.val = Snapshot.toSt lockedReg := by
    rw [hw2]; rfl
  exact e2 ▸ Reachable.step r1' h2

end cratesio_kernel.Scenarios
