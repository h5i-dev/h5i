import Theorems
/-!
# Before the fix, an old session outlives its key

`transition_pre` checks a key when a session is opened and trusts the
session's copy afterwards. The run `pre_run` issues key `k`, opens a session
with it, revokes `k`, and then acts through the session: the last request
succeeds on behalf of `k`, which `no_act_after_revoke` rules out for
`transition` (`pre_acts_after_revoke`). The fixed kernel refuses that request
(`fixed_refuses`) and allows it before the revocation (`fixed_run`).
-/
open Aeneas Aeneas.Std Result H5iAppLib keys_kernel

namespace Keys

def k0 : alloc.vec.Vec U8 := vecOf [107#u8]
def perms0 : alloc.vec.Vec Perm := vecOf [.Write]
def key0 : Key := ⟨k0, perms0⟩
def sess0 : Session := ⟨0#u64, k0, perms0⟩

def s0 : Snapshot := ⟨alloc.vec.Vec.new _, alloc.vec.Vec.new _, alloc.vec.Vec.new _, 0#u64⟩
def s1 : Snapshot := ⟨vecOf [key0], alloc.vec.Vec.new _, alloc.vec.Vec.new _, 0#u64⟩
def s2 : Snapshot := ⟨vecOf [key0], alloc.vec.Vec.new _, vecOf [sess0], 1#u64⟩
def s3 : Snapshot := ⟨alloc.vec.Vec.new _, vecOf [k0], vecOf [sess0], 1#u64⟩

abbrev Ev := Event Cred Command Reply keys_kernel.Error

def e1 : Ev := .ok .Admin (.Issue k0 perms0) .Done
def e2 : Ev := .ok (.Key k0) .Open (.Opened 0#u64)
def e3 : Ev := .ok .Admin (.Revoke k0) .Done
def e4 : Ev := .ok (.Session 0#u64) (.Act .Write) (.Acted k0)

theorem zero_ne_max : (0#u64) ≠ core.num.U64.MAX := by simp [core.num.U64.MAX, U64.rMax]

theorem wrapping_one : core.num.U64.wrapping_add 0#u64 1#u64 = 1#u64 := by decide

/-- A step of `f` from `s` that replies `r` and writes `l`. -/
def Writes (f : Cred → Snapshot → Command → Result (core.result.Result (alloc.vec.Vec Write × Reply) keys_kernel.Error))
    (a : Cred) (s : Snapshot) (c : Command) (r : Reply) (l : List Write) : Prop :=
  f a s c ⦃ res => ∃ ws, res = .Ok (ws, r) ∧ ws.val = l ⦄

theorem run_step {f : Cred → Snapshot → Command → Result (core.result.Result (alloc.vec.Vec Write × Reply) keys_kernel.Error)}
    {st : St} {evs : List Ev} {s : Snapshot} {a : Cred} {c : Command} {r : Reply} {l : List Write} {st' : St}
    (hr : Run f toSt applyAll init st evs) (hs : toSt s = st) (hw : Writes f a s c r l)
    (hst : l.foldl applyW st = st') : Run f toSt applyAll init st' (evs ++ [.ok a c r]) := by
  obtain ⟨res, hres, ws, rfl, hl⟩ := (WP.spec_equiv_exists _ _).1 hw
  have := Run.ok hr hs hres
  rwa [applyAll, hl, hst] at this

theorem issue_k0 (f) (hf : ∀ a s c, f a s c = step a s c false ∨ f a s c = step a s c true) :
    Writes f .Admin s0 (.Issue k0 perms0) .Done [.PutKey key0] := by
  unfold Writes
  rcases hf .Admin s0 (.Issue k0 perms0) with h | h <;> rw [h] <;> unfold step <;> step* <;>
    simp_all [takenM, findKey, s0, key0]

theorem pre_open : Writes transition_pre (.Key k0) s1 .Open (.Opened 0#u64) [.PutSession sess0] := by
  unfold Writes transition_pre step
  step*
  all_goals simp_all [resolvePreM, findKey, s1, key0, sess0, zero_ne_max]

theorem pre_revoke : Writes transition_pre .Admin s2 (.Revoke k0) .Done [.DelKey k0, .Revoked k0] := by
  unfold Writes transition_pre step
  step*

theorem pre_act : Writes transition_pre (.Session 0#u64) s3 (.Act .Write) (.Acted k0) [] := by
  unfold Writes transition_pre step
  step*
  all_goals simp_all [resolvePreM, s3, sess0, grantsM, permitsM, perms0, vec_deref_val]

/-- The run: issue `k`, open a session with it, revoke `k`, act through the session. -/
theorem pre_run : ∃ st, Run transition_pre toSt applyAll init st [e1, e2, e3, e4] := by
  have r1 := run_step (st' := toSt s1) Run.nil (s := s0) rfl
    (issue_k0 transition_pre (fun _ _ _ => .inr rfl)) (by simp [applyW, init, toSt, s1])
  have r2 := run_step (st' := toSt s2) r1 rfl pre_open (by simp [applyW, toSt, s1, s2, sess0, wrapping_one])
  have r3 := run_step (st' := toSt s3) r2 rfl pre_revoke (by simp [applyW, toSt, s2, s3, key0])
  have r4 := run_step r3 rfl pre_act rfl
  exact ⟨_, by simpa [e1, e2, e3, e4] using r4⟩

/-- `no_act_after_revoke` fails for `transition_pre`. -/
theorem pre_acts_after_revoke : ¬ ∀ st evs, Run transition_pre toSt applyAll init st evs →
    ∀ pre post, evs = pre ++ e3 :: post → e4 ∉ post := by
  intro h
  obtain ⟨st, hr⟩ := pre_run
  exact h st _ hr [e1, e2] [e4] rfl (by simp)

/-- The fixed kernel refuses the last request. -/
theorem fixed_refuses : transition (.Session 0#u64) s3 (.Act .Write) = ok (.Err .NoKey) := by
  unfold transition step
  have := resolve_spec s3 (.Session 0#u64)
  rw [show resolveM s3 (.Session 0#u64) = none by simp [resolveM, findKey, s3, sess0]] at this
  rw [eq_ok_of_spec this]; simp

theorem fixed_act : Writes transition (.Session 0#u64) s2 (.Act .Write) (.Acted k0) [] := by
  unfold Writes transition step
  step*
  all_goals simp_all [resolveM, findKey, s2, sess0, key0, grantsM, permitsM, perms0, vec_deref_val]

theorem fixed_open : Writes transition (.Key k0) s1 .Open (.Opened 0#u64) [.PutSession sess0] := by
  unfold Writes transition step
  step*
  all_goals simp_all [resolveM, findKey, s1, key0, sess0, zero_ne_max]

/-- The fixed kernel serves the session before the revocation, so the theorem
does not hold by refusing everything. -/
theorem fixed_run : ∃ st, Run transition toSt applyAll init st [e1, e2, e4] := by
  have r1 := run_step (st' := toSt s1) Run.nil (s := s0) rfl
    (issue_k0 transition (fun _ _ _ => .inl rfl)) (by simp [applyW, init, toSt, s1])
  have r2 := run_step (st' := toSt s2) r1 rfl fixed_open (by simp [applyW, toSt, s1, s2, sess0, wrapping_one])
  have r3 := run_step r2 rfl fixed_act rfl
  exact ⟨_, by simpa [e1, e2, e4] using r3⟩

end Keys
