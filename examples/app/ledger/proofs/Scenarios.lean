import Apply
/-!
# Scenarios

The tutorial's session on the extracted code: Alice and Bob open accounts,
Alice deposits 100 and moves 30 to Bob. The final state is reachable, and
each refusal the theorems rely on does happen.
-/
open Aeneas Aeneas.Std Result ledger_kernel ledger_kernel.Spec ledger_kernel.Commands
  ledger_kernel.Theorems H5iAppLib

namespace ledger_kernel.Scenarios

attribute [simp] u64_max_val

def alice : Principal := ⟨1#u64, 1#u64⟩
def bob : Principal := ⟨1#u64, 2#u64⟩

def s0 : Snapshot := ⟨⟨0#u64, 0#u64, 0#u64⟩, vecOf []⟩
-- Alice opens account 0.
def s1 : Snapshot := ⟨⟨1#u64, 0#u64, 0#u64⟩, vecOf [⟨0#u64, 1#u64, 0#u64⟩]⟩
-- Bob opens account 1.
def s2 : Snapshot := ⟨⟨2#u64, 0#u64, 0#u64⟩, vecOf [⟨0#u64, 1#u64, 0#u64⟩, ⟨1#u64, 2#u64, 0#u64⟩]⟩
-- Alice deposits 100.
def s3 : Snapshot := ⟨⟨2#u64, 100#u64, 0#u64⟩, vecOf [⟨0#u64, 1#u64, 100#u64⟩, ⟨1#u64, 2#u64, 0#u64⟩]⟩
-- Alice transfers 30 to Bob.
def s4 : Snapshot := ⟨⟨2#u64, 100#u64, 0#u64⟩, vecOf [⟨0#u64, 1#u64, 70#u64⟩, ⟨1#u64, 2#u64, 30#u64⟩]⟩

/-- `c` by `a` succeeds in `s` and leads to `s'`. -/
def Runs (s : Snapshot) (a : Principal) (c : Command) (s' : Snapshot) : Prop :=
  transition a s c ⦃ r => ∃ ws rep, r = .Ok (ws, rep) ∧ applyAll (Snapshot.toSt s) ws.val = Snapshot.toSt s' ⦄

theorem reachable_next {s s' : Snapshot} {a : Principal} {c : Command}
    (hr : Reachable (Snapshot.toSt s)) (h : Runs s a c s') : Reachable (Snapshot.toSt s') := by
  obtain ⟨x, hx, ws, rep, rfl, happ⟩ := (WP.spec_equiv_exists _ _).1 h
  exact happ ▸ Reachable.step hr hx

/-- Runs the kernel on concrete values: `simp` evaluates the lookups and
`step*` the arithmetic. -/
macro "run" : tactic => `(tactic| (
  unfold Runs
  simp [transition, «open», deposit, withdraw, transfer, eq_ok_of_spec (find_account_spec _ _), vecOf,
    s0, s1, s2, s3, alice, bob]
  step*
  all_goals first
    | (exfalso; have := congrArg UScalar.val ‹_ = core.num.U64.MAX›; simp [U64.rMax] at this)
    | (simp only [u64_max_val, U64.rMax] at *; scalar_tac)
    | (refine ⟨_, ⟨_, rfl⟩, ?_⟩
       simp [*, applyAll, applyWrite, Snapshot.toSt, s1, s2, s3, s4, vecOf, upsert]
       try (first | scalar_tac | (constructor <;> scalar_tac)))
    | simp))

theorem open_alice : Runs s0 alice .Open s1 := by run
theorem open_bob : Runs s1 bob .Open s2 := by run
theorem deposit_alice : Runs s2 alice (.Deposit 0#u64 100#u64) s3 := by run

/-- The owner's transfer succeeds and lowers her balance, so `only_owner_debits`
is about something that happens. -/
theorem transfer_alice : Runs s3 alice (.Transfer 0#u64 1#u64 30#u64) s4 := by run

theorem reachable_s3 : Reachable (Snapshot.toSt s3) :=
  reachable_next (reachable_next (reachable_next (by exact Reachable.init) open_alice) open_bob) deposit_alice

theorem reachable_s4 : Reachable (Snapshot.toSt s4) := reachable_next reachable_s3 transfer_alice

/-- The hypotheses of `transfer_succeeds` hold in `s3`. -/
theorem transfer_succeeds_s3 : ∃ ws r, transition alice s3 (.Transfer 0#u64 1#u64 30#u64) = ok (.Ok (ws, r)) :=
  transfer_succeeds alice s3 _ _ _ ⟨0#u64, 1#u64, 100#u64⟩ ⟨1#u64, 2#u64, 0#u64⟩ reachable_s3 (by decide)
    (by simp [findAcc, Snapshot.toSt, s3, vecOf]) rfl (by simp) (by simp [findAcc, Snapshot.toSt, s3, vecOf])

/-- Deposits 100, withdrawals 0, and the accounts hold 70 + 30. -/
theorem s4_conserved : total (Snapshot.toSt s4) = 100 ∧ (Snapshot.toSt s4).deposited = 100 := by
  simp [total, Snapshot.toSt, s4, vecOf]

/-! ## Refusals -/

theorem bob_cannot_withdraw : transition bob s3 (.Withdraw 0#u64 5#u64) = ok (.Err .Forbidden) := by
  simp [transition, withdraw, eq_ok_of_spec (find_account_spec _ _), vecOf, s3, bob]

theorem bob_cannot_transfer : transition bob s3 (.Transfer 0#u64 1#u64 5#u64) = ok (.Err .Forbidden) := by
  simp [transition, transfer, eq_ok_of_spec (find_account_spec _ _), vecOf, s3, bob]

theorem no_self_transfer : transition alice s3 (.Transfer 0#u64 0#u64 5#u64) = ok (.Err .SameAccount) := by
  simp [transition, transfer]

theorem no_overdraft : transition alice s3 (.Withdraw 0#u64 101#u64) = ok (.Err .Insufficient) := by
  simp [transition, withdraw, eq_ok_of_spec (find_account_spec _ _), vecOf, s3, alice]

/-- The deposit limit is real: total deposits cannot pass 2^64 - 1. -/
theorem deposit_overflow :
    transition alice s3 (.Deposit 0#u64 18446744073709551615#u64) = ok (.Err .Overflow) := by
  apply eq_ok_of_spec
  simp [transition, deposit, s3, alice, eq_ok_of_spec (find_account_spec _ _), vecOf]
  step*
  all_goals (simp only [u64_max_val, U64.rMax] at *; scalar_tac)

end ledger_kernel.Scenarios
